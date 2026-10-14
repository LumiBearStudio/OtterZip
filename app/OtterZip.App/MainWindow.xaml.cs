using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Windows.ApplicationModel.Resources;
using OtterZip.App.Dialogs;
using OtterZip.App.Models;
using OtterZip.App.Services;
using OtterZip.Interop;
using Windows.ApplicationModel.DataTransfer;

namespace OtterZip.App;

/// <summary>
/// Keka-pattern main window (2026-04-30).
///
/// The whole client area is a drop target. The visible body is the
/// <see cref="UserControls.ConfigPanel"/> (compression settings); when a
/// drag enters, the <c>DropOverlay</c> Border surfaces over the panel and
/// accepts/rejects based on payload type.
///
/// Drop dispatch:
///   - Single archive file → ExtractDialog (S5)
///   - Multiple archives  → bulk extract, each into its own folder
///   - Anything else      → compress with current ConfigPanel options (S3)
///   - Mixed (archives + plain files) → reject
/// </summary>
[System.Diagnostics.CodeAnalysis.SuppressMessage(
    "Design", "CA1001:Types that own disposable fields should be disposable",
    Justification = "WinUI 3 Window doesn't implement IDisposable. _jobQueue lives for the window lifetime; the SemaphoreSlim it owns is released when the process tears down.")]
public sealed partial class MainWindow : Window
{
    private readonly ResourceLoader _strings;
    private readonly JobQueue _jobQueue;

    // ============================================================
    //  View stack — Keka-style inline panels instead of modal dialogs
    // ============================================================
    private enum AppView { Idle, Extract }
    private AppView _currentView = AppView.Idle;
    // Pending extract submission lives here until OnExtractSubmitted /
    // OnExtractDismissed flips it. Lets us keep ExtractAsync
    // synchronous-looking while the user interacts with the panel.
    private TaskCompletionSource<UserControls.ExtractPanelSubmitArgs?>? _pendingExtractTcs;

    public MainWindow()
    {
        InitializeComponent();
        _strings = new ResourceLoader();
        Title = _strings.GetString("App_WindowTitle");

        // JobQueue is the home for compress/extract work. The concurrent
        // limit follows Settings_ConcurrentJobs (1-4). Default 2 reflects
        // the common case of "drop a few archives at once and want them
        // moving in parallel" — single-core / contention-sensitive users
        // can drop it to 1 in Settings.
        int concurrency = Math.Clamp(
            SettingsService.Get<int>("Settings_ConcurrentJobs", 2), 1, 4);
        _jobQueue = new JobQueue(DispatcherQueue, concurrency);

        WireConcurrentJobsLiveUpdate();
        _jobQueue.JobSettled += OnJobSettled;
        FloatLayerHost.Attach(_jobQueue);

        if (Content is FrameworkElement root)
        {
            ThemeService.Apply(root, ThemeService.Load());
        }

        // Phase 6+ rev 6: unified Mica across both windows — see ApplyBackdrop.
        ApplyBackdrop();

        // Custom title bar: hand the OS our empty Border as the drag region.
        // The OS still draws min/max/close on the right; everything to the
        // left of those is draggable, and the rest of the window is free
        // to host content without colliding with caption buttons.
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        // Compact Keka-style window — rev 5 tightened further now that
        // Method/advanced options moved behind an Expander.
        TrySizeWindow(width: 420, height: 460);

        // Click on the drop-hint card → reuse the file-picker flow that
        // backs the right-click "Add files…" menu.
        ConfigPanel.DropHintTapped += async (_, _) =>
            await PickFilesAndProcessAsync().ConfigureAwait(false);
        // Phase 4: gear chip in the corner strip replaces the bottom
        // status-bar settings button. Forward to the same handler so
        // both routes open the same Settings surface.
        ConfigPanel.SettingsRequested += (s, _) =>
            OnSettingsButtonClick(s ?? this, new RoutedEventArgs());

        // Window-wide drag/drop. Wired once in ctor — the root grid never
        // changes its lifetime so detaching is unnecessary.
        RootGrid.DragEnter += OnDragEnter;
        RootGrid.DragOver += OnDragOver;
        RootGrid.DragLeave += OnDragLeave;
        RootGrid.Drop += OnDrop;

        WireExtractPanel();

        // Phase 6+ rev 4: hook AppWindow.Closing so an in-flight job can
        // prompt the user before the window vanishes. WinUI 3 Window.Closed
        // is non-cancellable; AppWindow.Closing carries `args.Cancel`.
        WireAppWindow();
    }

    /// <summary>
    /// Live-update hook for Settings_ConcurrentJobs. When the user bumps
    /// the setting in Settings UI, raise the JobQueue's limit on the
    /// spot so already-queued cards start moving without an app restart.
    /// Lowering still needs a restart — we can't yank a slot from work
    /// already running.
    /// </summary>
    private void WireConcurrentJobsLiveUpdate()
    {
        SettingsService.Changed += (_, args) =>
        {
            if (!string.Equals(args.Key, "Settings_ConcurrentJobs", StringComparison.Ordinal))
            {
                return;
            }
            int n = Math.Clamp(
                SettingsService.Get<int>("Settings_ConcurrentJobs", 2), 1, 4);
            _jobQueue.TrySetConcurrentLimit(n);
        };
    }

    /// <summary>
    /// Inline extract panel event wiring. Completes <c>_pendingExtractTcs</c>
    /// when the user submits or cancels; LayoutChanged re-fits the window
    /// after the "다른 폴더로…" toggle expands the destination row.
    /// </summary>
    private void WireExtractPanel()
    {
        ExtractPanel.Submitted += OnExtractSubmitted;
        ExtractPanel.Dismissed += OnExtractDismissed;
        ExtractPanel.PasswordEdited += (_, _) => ExtractPanel.ClearError();
        // ExtractPanel.LayoutChanged used to resize the host window
        // when the destination row toggled. The popup-card refactor
        // keeps the window fixed and lets the card grow/shrink in
        // place, so the handler is no-op now — left unsubscribed.
    }

    /// <summary>
    /// Bind to the underlying AppWindow for both the Closing-confirm hook
    /// and the otter mascot icon. Best-effort: if either step fails the
    /// window still works, just without the close prompt or custom icon.
    /// </summary>
    private void WireAppWindow()
    {
        try
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
            var windowId = Microsoft.UI.Win32Interop.GetWindowIdFromWindow(hwnd);
            var appWindow = Microsoft.UI.Windowing.AppWindow.GetFromWindowId(windowId);
            appWindow.Closing += OnAppWindowClosing;
            TrySetWindowIcon(appWindow);
        }
        catch (Exception)
        {
            // Best-effort — closing-confirm degrades to "always allow close".
        }
    }

    /// <summary>
    /// Apply the otter mascot to the window. AppWindow.SetIcon resolves
    /// relative paths against the *working directory*, not the EXE — which
    /// burns us when VS launches the unpackaged build with a different CWD.
    /// We resolve against AppContext.BaseDirectory and pre-check existence
    /// so the call site stays predictable; the all-Exception catch keeps a
    /// future WindowsAppSDK regression from crashing the app over a purely
    /// cosmetic asset.
    /// </summary>
    internal static void TrySetWindowIcon(Microsoft.UI.Windowing.AppWindow appWindow)
    {
        try
        {
            string iconPath = System.IO.Path.Combine(
                AppContext.BaseDirectory, "Assets", "AppIcon.ico");
            if (System.IO.File.Exists(iconPath))
            {
                appWindow.SetIcon(iconPath);
            }
        }
        catch (Exception)
        {
            // Cosmetic asset — falls back to the EXE-embedded ApplicationIcon.
        }
    }

    /// <summary>
    /// Phase 6+ rev 6: unified Mica backdrop across MainWindow + SettingsWindow.
    /// Earlier rev used DesktopAcrylicBackdrop here while Settings used Mica,
    /// which produced a visible tone mismatch (main = nearly black due to
    /// wallpaper bleed-through, Settings = navy-grey from Mica's tonal palette).
    /// WinUI 3 design guidance: long-lived windows use Mica; Acrylic is reserved
    /// for transient surfaces (flyouts, command bars).
    /// Kind=Base is the standard for primary windows. SettingsWindow uses
    /// BaseAlt because it hosts a NavigationView pane — both Kinds derive from
    /// the same wallpaper palette so the perceived tone matches.
    /// Falls back automatically to a solid color on Windows builds without
    /// composition support — no try/catch needed at this layer.
    /// </summary>
    private void ApplyBackdrop()
    {
        SystemBackdrop = new Microsoft.UI.Xaml.Media.MicaBackdrop
        {
            Kind = Microsoft.UI.Composition.SystemBackdrops.MicaKind.Base,
        };
    }

    private bool _confirmedExit;
    // Set when PromptExitWhileBusyAsync fails to even show the dialog. A
    // second consecutive failure fails OPEN so the user is never trapped
    // unable to close the window (APP-M2).
    private bool _exitPromptFailed;

    /// <summary>
    /// True when the JobQueue has at least one running or queued card —
    /// used by OnAppWindowClosing to decide whether to prompt.
    /// </summary>
    private bool HasActiveQueueJobs()
    {
        foreach (var job in _jobQueue.Jobs)
        {
            if (job.State == JobState.Running || job.State == JobState.Queued)
            {
                return true;
            }
        }
        return false;
    }

    private void CancelAllQueueJobs()
    {
        foreach (var job in _jobQueue.Jobs)
        {
            if (job.State == JobState.Running || job.State == JobState.Queued)
            {
                job.RequestCancel();
            }
        }
    }

    /// <summary>
    /// Hook for JobQueue's JobSettled event — fires on the dispatcher
    /// thread after any terminal transition. Wired in the constructor.
    ///
    /// Successful jobs trigger three opt-in follow-ups, gated by Settings:
    ///   * Toast notification (<c>Settings_ShowToast</c>, default ON)
    ///   * Reveal-in-Explorer (<c>Settings_RevealAfter*</c>, default ON)
    ///   * Recycle source archive after extract
    ///     (<c>Settings_DeleteArchiveAfterExtract</c>, default OFF)
    /// All three live in this single hook so the queue's work delegates
    /// stay focused on the actual archive op — side effects only run
    /// once per Done transition, never on Error/Cancelled.
    /// </summary>
    private void OnJobSettled(object? sender, JobItem item)
    {
        SoundService.MaybePlayCompletion(item);
        if (item.State == JobState.Done && !string.IsNullOrEmpty(item.ResultPath))
        {
            TryToastCompletion(item);
            // Shell-invoked (right-click) ops don't reveal — the user is
            // already in the Explorer view that the verb fired from. The
            // toast still fires so there's a completion signal.
            if (!_launchedFromShellInvoke)
            {
                TryRevealOnCompletion(item);
            }
            TryRecycleSourceArchive(item);
        }
        // "빠른 작업 완료 후 창 자동 닫기": if this window opened only to service a
        // right-click verb and the whole batch succeeded, close it. Runs on every
        // settle (not only Done) so a trailing failure keeps the window open.
        MaybeScheduleAutoClose();
    }

    private void TryToastCompletion(JobItem item)
    {
        try
        {
            // Neutral "{name} · {detail}" body. The status-bar formats used
            // before expect (entryCount, size) — feeding them (folderName,
            // fullStatusText) produced a garbled double sentence in every
            // locale ("MyFolder개 항목 추출 완료 · 154개 항목 추출 완료 · …").
            string body = string.Format(CultureInfo.CurrentCulture,
                _strings.GetString("Toast_CompressBodyFormat/Text"),
                Path.GetFileName(item.ResultPath!),
                item.StatusText ?? string.Empty);
            ToastService.ShowCompletion(item.DisplayName, body);
        }
        catch
        {
            // Toasts are decoration — never let them break the run.
        }
    }

    /// <summary>
    /// Show the result in Explorer when the corresponding "reveal after"
    /// setting is on. Compress reveals the archive file itself (selected
    /// in its parent folder); extract reveals the destination folder.
    /// </summary>
    private static void TryRevealOnCompletion(JobItem item)
    {
        string key = item.Kind == JobKind.Compress
            ? "Settings_RevealAfterCompress"
            : "Settings_RevealAfterExtract";
        if (!SettingsService.Get<bool>(key, true)) return;
        try
        {
            Win32Helper.RevealInExplorer(item.ResultPath!);
        }
        catch
        {
            // Reveal is a convenience — never let it break the queue.
        }
    }

    /// <summary>
    /// Move the source archive to the Recycle Bin after a successful
    /// extract when <c>Settings_DeleteArchiveAfterExtract</c> is on. The
    /// source path is captured at submit time on <see cref="JobItem.SourcePath"/>
    /// — relying on local state in the work delegate would race the
    /// completion handler. Compress jobs have no single source archive
    /// here (their sources are handled inline via
    /// <c>MaybeRecycleSources</c>), so this branch is extract-only.
    /// </summary>
    private static void TryRecycleSourceArchive(JobItem item)
    {
        if (item.Kind != JobKind.Extract) return;
        if (string.IsNullOrEmpty(item.SourcePath)) return;
        if (!SettingsService.Get<bool>("Settings_DeleteArchiveAfterExtract", false)) return;
        try
        {
            Win32Helper.MoveToRecycleBin(item.SourcePath);
        }
        catch
        {
            // Recycle is best-effort; file locks / permissions stay out
            // of the user's face — the extract itself already succeeded.
        }
    }

    // Guards a multi-archive batch from stacking auto-close timers.
    private bool _autoCloseScheduled;

    /// <summary>
    /// "빠른 작업 완료 후 창 자동 닫기" (<c>Settings_QuickProgressAutoClose</c>) —
    /// when this MainWindow exists only to service an Explorer right-click verb
    /// (<see cref="_launchedFromShellInvoke"/>) and every queued job finished in
    /// <see cref="JobState.Done"/>, close the window after a short delay so the
    /// app doesn't linger (matches the quick-compress ProgressDialog's 2s
    /// auto-close, JobProgressView.MaybeStartAutoCloseTimer). Any Error/Cancelled
    /// job — or one still Running/Queued — leaves the window open so the user can
    /// see what happened.
    /// </summary>
    private void MaybeScheduleAutoClose()
    {
        if (_autoCloseScheduled) return;
        if (!_launchedFromShellInvoke) return;
        if (!SettingsService.Get<bool>("Settings_QuickProgressAutoClose", false)) return;
        foreach (var job in _jobQueue.Jobs)
        {
            if (job.State != JobState.Done) return;
        }
        _autoCloseScheduled = true;
        var timer = DispatcherQueue.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(2);
        timer.IsRepeating = false;
        timer.Tick += (_, _) =>
        {
            timer.Stop();
            // A new drop / operation started during the delay cancels the close.
            if (HasActiveQueueJobs())
            {
                _autoCloseScheduled = false;
                return;
            }
            try { this.Close(); }
            catch { /* window may already be tearing down */ }
        };
        timer.Start();
    }

    private async void OnAppWindowClosing(
        Microsoft.UI.Windowing.AppWindow sender,
        Microsoft.UI.Windowing.AppWindowClosingEventArgs args)
    {
        // Already confirmed once — let the second event through.
        if (_confirmedExit) { TryCloseSubWindows(); return; }
        // No active job — nothing to confirm.
        if (!HasActiveQueueJobs()) { TryCloseSubWindows(); return; }
        // User opted out — let it close.
        if (!SettingsService.Get<bool>("Settings_ConfirmExitWhileBusy", true))
        {
            TryCloseSubWindows();
            return;
        }

        // Cancel the OS close until the user confirms. We re-raise Close()
        // ourselves once they say yes; otherwise we leave the window open.
        args.Cancel = true;
        bool quit = await PromptExitWhileBusyAsync().ConfigureAwait(true);
        if (quit)
        {
            // Cancel any in-flight queue card so the OS hand-off below
            // doesn't strand work mid-flight.
            CancelAllQueueJobs();
            _confirmedExit = true;
            TryCloseSubWindows();
            try { this.Close(); }
            catch (InvalidOperationException) { /* already torn down */ }
            catch (System.Runtime.InteropServices.COMException) { /* HWND gone */ }
        }
    }

    /// <summary>
    /// Best-effort close of any auxiliary windows MainWindow owns. Called
    /// from the AppWindow.Closing handler so child windows tear down while
    /// MainWindow.Content is still alive — their Closed handlers can then
    /// safely touch our Content (theme re-apply etc.).
    /// </summary>
    private void TryCloseSubWindows()
    {
        try { _settingsWindow?.Close(); }
        catch (InvalidOperationException) { /* already closed */ }
        catch (System.Runtime.InteropServices.COMException) { /* HWND gone */ }
    }

    private async Task<bool> PromptExitWhileBusyAsync()
    {
        var dialog = new ContentDialog
        {
            Title = _strings.GetString("ExitConfirm_Title/Text"),
            Content = _strings.GetString("ExitConfirm_Body/Text"),
            PrimaryButtonText = _strings.GetString("ExitConfirm_Quit/Text"),
            CloseButtonText = _strings.GetString("ExitConfirm_Stay/Text"),
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = Content.XamlRoot,
        };
        try
        {
            var result = await dialog.ShowAsync();
            _exitPromptFailed = false; // prompt worked — reset the escape hatch
            return result == ContentDialogResult.Primary;
        }
        catch (Exception)
        {
            // WinUI allows only ONE ContentDialog per XamlRoot — usually
            // another dialog (e.g. the compress password prompt) is open, so
            // ShowAsync throws. Treat the FIRST failure as "stay": the visible
            // dialog keeps the user's attention, and dismissing it lets the
            // next X-click prompt normally. But never TRAP the user unable to
            // close: if showing the prompt fails AGAIN on the next attempt,
            // fail OPEN and honor the close — OnAppWindowClosing still cancels
            // in-flight jobs cleanly (APP-M2).
            if (_exitPromptFailed)
            {
                return true;
            }
            _exitPromptFailed = true;
            return false;
        }
    }

    private void TrySizeWindow(int width, int height)
    {
        try
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
            var windowId = Microsoft.UI.Win32Interop.GetWindowIdFromWindow(hwnd);
            var appWindow = Microsoft.UI.Windowing.AppWindow.GetFromWindowId(windowId);
            uint dpi = GetDpiForWindow(hwnd);
            double scale = dpi / 96.0;
            int physW = (int)(width * scale);
            int physH = (int)(height * scale);
            appWindow.Resize(new Windows.Graphics.SizeInt32(physW, physH));

            // Phase 6+ rev 5: lock the window to the chosen compact size.
            // OtterZip is a launcher / config surface, not a workspace —
            // resizing or maximizing actively hurts the layout. We keep
            // the minimize button so users can stash it out of the way.
            if (appWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter presenter)
            {
                presenter.IsResizable = false;
                presenter.IsMaximizable = false;
            }
        }
        catch (Exception)
        {
            // Best-effort — fall back to OS-default sizing.
        }
    }

    [System.Runtime.InteropServices.DllImport("user32.dll", ExactSpelling = true)]
    private static extern uint GetDpiForWindow(IntPtr hwnd);

    // ============================================================
    //  Shell extension entry (--invoke verb)
    // ============================================================

    /// <summary>
    /// True when this MainWindow was launched to service an Explorer
    /// right-click verb (<c>--invoke</c>). Suppresses the post-job
    /// reveal-in-Explorer: the user is already looking at the folder
    /// in the Explorer window they right-clicked from, so popping a
    /// second Explorer window selecting the new archive is redundant
    /// and jarring (Bandizip doesn't do it either). Drag &amp; drop /
    /// in-app operations keep the Settings_RevealAfter* behavior.
    /// </summary>
    private bool _launchedFromShellInvoke;

    internal void PreloadInvoke(InvokeRequest request)
    {
        _launchedFromShellInvoke = true;
        DispatcherQueue.TryEnqueue(() =>
        {
            _ = DispatchInvokeAsync(request);
        });
    }

    /// <summary>
    /// Route files opened via a file association / "Open with" into the
    /// same drop pipeline as dragged files (extract archives, compress
    /// other inputs). Unlike <see cref="PreloadInvoke"/> this is a normal
    /// user-initiated open from Explorer, so the usual reveal-after /
    /// sound settings apply (no shell-invoke suppression).
    /// </summary>
    internal void PreloadFiles(IReadOnlyList<string> paths)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            _ = ForwardPathsAsync(paths);
        });
    }

    private async Task DispatchInvokeAsync(InvokeRequest request)
    {
        try
        {
            if (string.Equals(request.Verb, "extract-here", StringComparison.OrdinalIgnoreCase)
                || string.Equals(request.Verb, "extract", StringComparison.OrdinalIgnoreCase))
            {
                await ExtractEachAsync(request.Paths, ShellExtractMode.Here).ConfigureAwait(true);
            }
            else if (string.Equals(request.Verb, "extract-smart", StringComparison.OrdinalIgnoreCase))
            {
                await ExtractEachAsync(request.Paths, ShellExtractMode.Smart).ConfigureAwait(true);
            }
            else if (string.Equals(request.Verb, "extract-to-subfolder", StringComparison.OrdinalIgnoreCase))
            {
                await ExtractEachAsync(request.Paths, ShellExtractMode.Subfolder).ConfigureAwait(true);
            }
            else if (string.Equals(request.Verb, "extract-dialog", StringComparison.OrdinalIgnoreCase))
            {
                await ExtractEachAsync(request.Paths, ShellExtractMode.Dialog).ConfigureAwait(true);
            }
            else if (string.Equals(request.Verb, "compress", StringComparison.OrdinalIgnoreCase))
            {
                await CompressAsync(request.Paths).ConfigureAwait(true);
            }
            else if (string.Equals(request.Verb, "compress-individually", StringComparison.OrdinalIgnoreCase))
            {
                // Bandizip 각 항목별 압축 — emit N archives, one per
                // selected input. Sequential through CompressAsync so
                // progress + cleanup paths stay identical. Default
                // format comes from Settings_DefaultMethodIndex via
                // PlanCompress, which CompressAsync already honors.
                foreach (var path in request.Paths)
                {
                    await CompressAsync(new[] { path }).ConfigureAwait(true);
                }
            }
            else
            {
                string msg = string.Format(CultureInfo.CurrentCulture,
                    _strings.GetString("Main_StatusBarUnknownInvokeFormat"),
                    request.Verb);
                _jobQueue.ReportError(JobKind.Compress, "OtterZip", msg);
            }
        }
        catch (Exception ex)
        {
            _jobQueue.ReportError(JobKind.Compress, "OtterZip", ErrorMessages.Localize(ex));
        }
    }

    /// <summary>
    /// Extract every selected archive with the given mode. Bandizip
    /// parity (2026-06-17): a multi-select Extract verb unpacks ALL of
    /// the chosen archives, not just the first. Each archive runs
    /// through <see cref="ExtractAsync"/> independently so per-archive
    /// destination logic (Here / Smart-layout / Subfolder) is applied
    /// to each. One archive failing doesn't abort the rest — the error
    /// surfaces on its own JobCard and the loop continues.
    /// </summary>
    private async Task ExtractEachAsync(IReadOnlyList<string> paths, ShellExtractMode mode)
    {
        foreach (string path in paths)
        {
            try
            {
                await ExtractAsync(path, mode).ConfigureAwait(true);
            }
            catch (Exception ex)
            {
                _jobQueue.ReportError(JobKind.Extract, Path.GetFileName(path), ErrorMessages.Localize(ex));
            }
        }
    }

    // ============================================================
    //  Window-wide drag & drop
    // ============================================================
    private DropClassification _currentDrop = DropClassification.None;

    private async void OnDragEnter(object sender, DragEventArgs e)
    {
        if (!e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = DataPackageOperation.None;
            ShowOverlayReject(_strings.GetString("DropOverlay_MixedRejectText/Text"));
            return;
        }

        // Peeking the storage items requires an async call; defer with a
        // deferral so the framework keeps the drag operation alive while
        // we classify the payload.
        var deferral = e.GetDeferral();
        try
        {
            var items = await e.DataView.GetStorageItemsAsync();
            var paths = items
                .Where(static i => i is not null)
                .Select(static i => i.Path)
                .Where(static p => !string.IsNullOrEmpty(p))
                .ToList();

            _currentDrop = ClassifyDrop(paths);
            switch (_currentDrop)
            {
                case DropClassification.CompressBatch:
                    e.AcceptedOperation = DataPackageOperation.Copy;
                    ShowOverlayAccept(FormatCompressSubtitle(paths.Count));
                    break;
                case DropClassification.ExtractSingle:
                    e.AcceptedOperation = DataPackageOperation.Copy;
                    ShowOverlayAccept(string.Format(CultureInfo.CurrentCulture,
                        _strings.GetString("DropOverlay_WillExtractFormat"),
                        Path.GetFileName(paths[0])));
                    break;
                case DropClassification.ExtractMultiple:
                    e.AcceptedOperation = DataPackageOperation.Copy;
                    ShowOverlayAccept(string.Format(CultureInfo.CurrentCulture,
                        _strings.GetString("DropOverlay_WillExtractMultipleFormat"),
                        paths.Count));
                    break;
                case DropClassification.Mixed:
                    e.AcceptedOperation = DataPackageOperation.None;
                    ShowOverlayReject(_strings.GetString("DropOverlay_MixedRejectText/Text"));
                    break;
                default:
                    e.AcceptedOperation = DataPackageOperation.None;
                    ShowOverlayReject(_strings.GetString("DropOverlay_EmptyReject/Text"));
                    break;
            }
        }
        finally
        {
            deferral.Complete();
        }
    }

    private void OnDragOver(object sender, DragEventArgs e)
    {
        // Re-affirm the operation kind so Windows doesn't downgrade it on
        // long hovers. Cheap; based on the cached classification.
        e.AcceptedOperation = _currentDrop is DropClassification.CompressBatch
            or DropClassification.ExtractSingle
            or DropClassification.ExtractMultiple
            ? DataPackageOperation.Copy
            : DataPackageOperation.None;
    }

    private void OnDragLeave(object sender, DragEventArgs e)
    {
        HideOverlay();
        _currentDrop = DropClassification.None;
    }

    private async void OnDrop(object sender, DragEventArgs e)
    {
        // Two-phase drop handler. See HarvestDropAsync for the why.
        var harvest = await HarvestDropAsync(e).ConfigureAwait(true);
        if (harvest.Paths is null)
        {
            DebugLog.Info("OnDrop: harvest returned null paths");
            return;
        }
        DebugLog.Info("OnDrop: classification=" + harvest.Classification + ", pathCount=" + harvest.Paths.Count + ", forceDialog=" + harvest.ForceDialog);

        try
        {
            switch (harvest.Classification)
            {
                case DropClassification.ExtractSingle:
                    // ForceDialog flips Keka-style silent extract to the
                    // inline panel for this one drop only — set when the
                    // user held Ctrl or Alt during release.
                    await ExtractAsync(harvest.Paths[0], forceDialog: harvest.ForceDialog).ConfigureAwait(true);
                    break;
                case DropClassification.ExtractMultiple:
                    await ExtractManyAsync(harvest.Paths).ConfigureAwait(true);
                    break;
                case DropClassification.CompressBatch:
                    await CompressAsync(harvest.Paths).ConfigureAwait(true);
                    break;
                // Mixed / Empty already rejected by overlay; no-op here.
            }
        }
        catch (Exception ex)
        {
            _jobQueue.ReportError(JobKind.Compress, "OtterZip", ErrorMessages.Localize(ex));
        }
    }

    private readonly record struct DropHarvest(
        IReadOnlyList<string>? Paths,
        DropClassification Classification,
        bool ForceDialog);

    /// <summary>
    /// Phase 1 of the drop pipeline: gather paths inside the OLE deferral
    /// and complete the deferral BEFORE returning. Caller (OnDrop) then
    /// runs the long-lived dialog work outside the drop event, which lets
    /// Windows release its drop animation immediately. Without this split
    /// the blue "Copy/복사" cursor with arrow stays pinned on top of the
    /// Extract dialog until the user clicks through.
    /// </summary>
    private async Task<DropHarvest> HarvestDropAsync(DragEventArgs e)
    {
        // Capture modifier state up-front: WinUI's DragDropModifiers
        // reflects keys held at the moment of the drop (NOT current
        // keyboard state). Ctrl OR Alt routes the drop through the
        // inline panel even when Settings_AskBeforeExtract is off.
        bool forceDialog = (e.Modifiers
            & (Windows.ApplicationModel.DataTransfer.DragDrop.DragDropModifiers.Control
               | Windows.ApplicationModel.DataTransfer.DragDrop.DragDropModifiers.Alt))
            != Windows.ApplicationModel.DataTransfer.DragDrop.DragDropModifiers.None;

        var deferral = e.GetDeferral();
        try
        {
            HideOverlay();
            if (!e.DataView.Contains(StandardDataFormats.StorageItems))
            {
                return default;
            }
            var items = await e.DataView.GetStorageItemsAsync();
            var paths = items
                .Where(static i => i is not null)
                .Select(static i => i.Path)
                .Where(static p => !string.IsNullOrEmpty(p))
                .ToList();
            var classification = ClassifyDrop(paths);
            _currentDrop = DropClassification.None;
            return new DropHarvest(paths, classification, forceDialog);
        }
        catch (Exception ex)
        {
            _jobQueue.ReportError(JobKind.Compress, "OtterZip", ErrorMessages.Localize(ex));
            return default;
        }
        finally
        {
            deferral.Complete();
        }
    }

    // ============================================================
    //  Drop classification
    // ============================================================
    private enum DropClassification { None, CompressBatch, ExtractSingle, ExtractMultiple, Mixed }

    private static DropClassification ClassifyDrop(IReadOnlyList<string> paths)
    {
        if (paths.Count == 0) return DropClassification.None;

        int archives = 0;
        int plains = 0;
        foreach (var p in paths)
        {
            if (Directory.Exists(p))
            {
                plains++;
            }
            else if (IsKnownArchive(p))
            {
                archives++;
            }
            else
            {
                plains++;
            }
        }

        // Phase 6+ rev 4: Settings_DefaultAction overrides the auto path
        // when the user has explicitly chosen "always compress" or
        // "always extract". "Mixed" still rejects in either mode — there's
        // no sensible single-action interpretation of a mixed payload.
        string action = SettingsService.Get<string>("Settings_DefaultAction", "auto");
        if (archives > 0 && plains > 0) return DropClassification.Mixed;

        if (string.Equals(action, "compress", StringComparison.Ordinal))
        {
            // Force compress even if the payload is an archive — user knows
            // they want to nest the archive into another archive.
            return DropClassification.CompressBatch;
        }
        if (string.Equals(action, "extract", StringComparison.Ordinal))
        {
            // Force extract — only meaningful when there are archives.
            // Plain-files-only with this setting becomes a no-op (None).
            if (archives == 1) return DropClassification.ExtractSingle;
            if (archives > 1) return DropClassification.ExtractMultiple;
            return DropClassification.None;
        }

        // Auto (default).
        if (archives == 1) return DropClassification.ExtractSingle;
        if (archives > 1) return DropClassification.ExtractMultiple;
        return DropClassification.CompressBatch;
    }

    // internal: App.OnLaunched classifies file-activation inputs with this
    // to route archive double-clicks to the focused ExtractOptionsDialog.
    internal static bool IsKnownArchive(string path)
    {
        try
        {
            var fmt = Archive.DetectFormat(path);
            return fmt switch
            {
                ArchiveFormat.Zip => true,
                ArchiveFormat.SevenZ => true,
                ArchiveFormat.Rar => true,
                ArchiveFormat.Tar => true,
                ArchiveFormat.TarGz => true,
                ArchiveFormat.TarBz2 => true,
                ArchiveFormat.TarXz => true,
                // Phase 7+ option Y -- mainstream cover extension.
                // Single-stream + tar wrappers + ZIP variants + ISO /
                // CAB / MSI / DEB. Drop-zone classification needs to
                // recognise every read-side ArchiveFormat the
                // dispatcher accepts; otherwise the user's drag of a
                // perfectly valid `.zst` / `.cab` / etc. would route
                // to the compress flow instead of extract.
                ArchiveFormat.Bzip2 => true,
                ArchiveFormat.Xz => true,
                ArchiveFormat.Lzma => true,
                ArchiveFormat.Zstd => true,
                ArchiveFormat.TarZst => true,
                ArchiveFormat.Lz4 => true,
                ArchiveFormat.TarLz4 => true,
                ArchiveFormat.Zipx => true,
                ArchiveFormat.Iso => true,
                ArchiveFormat.Cab => true,
                ArchiveFormat.Msi => true,
                ArchiveFormat.Deb => true,
                _ => false,
            };
        }
        catch
        {
            return false;
        }
    }

    private string FormatCompressSubtitle(int itemCount)
    {
        string fmt = _strings.GetString("DropOverlay_WillCompressFormat");
        return string.Format(CultureInfo.CurrentCulture, fmt, itemCount, ConfigPanel.SelectedFormat);
    }

    // ============================================================
    //  Drop overlay visibility helpers
    // ============================================================
    private void ShowOverlayAccept(string subtitle)
    {
        DropOverlay.Visibility = Visibility.Visible;
        DropOverlay.Background = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources["DropOverlayBackgroundBrush"];
        DropOverlayCard.BorderBrush = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources["OtterzipBrandBrush"];
        DropOverlayIcon.Foreground = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources["OtterzipBrandBrush"];
        DropOverlayTitle.Foreground = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources["OtterzipBrandBrush"];
        // Restore the inviting title — a previous reject may have swapped it.
        DropOverlayTitle.Text = _strings.GetString("DropOverlay_HoverTitle/Text");
        DropOverlaySubtitle.Text = subtitle;
    }

    private void ShowOverlayReject(string subtitle)
    {
        DropOverlay.Visibility = Visibility.Visible;
        DropOverlay.Background = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources["DropOverlayRejectBackgroundBrush"];
        DropOverlayCard.BorderBrush = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources["SystemFillColorCriticalBrush"];
        DropOverlayIcon.Foreground = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources["SystemFillColorCriticalBrush"];
        DropOverlayTitle.Foreground = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources["SystemFillColorCriticalBrush"];
        // A red card titled "여기에 놓으세요" contradicted itself — say why
        // the drop is refused instead.
        DropOverlayTitle.Text = _strings.GetString("DropOverlay_RejectTitle/Text");
        DropOverlaySubtitle.Text = subtitle;
    }

    private void HideOverlay()
    {
        DropOverlay.Visibility = Visibility.Collapsed;
    }

    // ============================================================
    //  Right-click menu
    // ============================================================
    private async void OnContextAddFiles(object sender, RoutedEventArgs e)
        => await PickFilesAndProcessAsync().ConfigureAwait(false);

    private async void OnContextAddFolder(object sender, RoutedEventArgs e)
        => await PickFolderAndProcessAsync().ConfigureAwait(false);

    private async void OnContextOpenSettings(object sender, RoutedEventArgs e)
        => await ShowSettingsAsync().ConfigureAwait(false);

    private async void OnSettingsButtonClick(object sender, RoutedEventArgs e)
        => await ShowSettingsAsync().ConfigureAwait(false);

    private async Task PickFilesAndProcessAsync()
    {
        var picker = new Windows.Storage.Pickers.FileOpenPicker();
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        picker.FileTypeFilter.Add("*");
        picker.SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.Desktop;
        var files = await picker.PickMultipleFilesAsync();
        if (files is null || files.Count == 0) return;
        var paths = files.Select(f => f.Path).ToList();
        await ForwardPathsAsync(paths).ConfigureAwait(true);
    }

    private async Task PickFolderAndProcessAsync()
    {
        var picker = new Windows.Storage.Pickers.FolderPicker();
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        picker.FileTypeFilter.Add("*");
        picker.SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.Desktop;
        var folder = await picker.PickSingleFolderAsync();
        if (folder is null) return;
        await ForwardPathsAsync(new[] { folder.Path }).ConfigureAwait(true);
    }

    private async Task ForwardPathsAsync(IReadOnlyList<string> paths)
    {
        switch (ClassifyDrop(paths))
        {
            case DropClassification.ExtractSingle:
                await ExtractAsync(paths[0]).ConfigureAwait(false);
                break;
            case DropClassification.ExtractMultiple:
                await ExtractManyAsync(paths).ConfigureAwait(false);
                break;
            case DropClassification.CompressBatch:
                await CompressAsync(paths).ConfigureAwait(false);
                break;
            // Mixed / None: silently ignore — picker UI doesn't usually permit this.
        }
    }

    private SettingsWindow? _settingsWindow;

    private Task ShowSettingsAsync()
    {
        // Phase 6+ rev 2: standalone window instead of in-process dialog.
        // We track the instance so re-clicking the gear focuses the
        // existing window rather than opening a duplicate.
        if (_settingsWindow is null)
        {
            _settingsWindow = new SettingsWindow();
            _settingsWindow.Closed += (_, _) =>
            {
                // Re-apply the theme once Settings closes — the user may
                // have flipped it inside General.
                //
                // Guard: if MainWindow itself was closed first (Settings
                // left open as a child surface), the WinUI runtime tears
                // SettingsWindow down too — its Closed handler then fires
                // while `this.Content` is already invalidated, throwing
                // "The WinUI Desktop Window object has already been closed".
                // We swallow that case so app shutdown stays clean.
                try
                {
                    if (Content is FrameworkElement root)
                    {
                        ThemeService.Apply(root, ThemeService.Load());
                    }
                }
                catch (InvalidOperationException)
                {
                    // MainWindow already torn down — nothing to re-theme.
                }
                catch (System.Runtime.InteropServices.COMException)
                {
                    // Underlying HWND gone (same root cause as above on
                    // some Windows builds). Safe to ignore.
                }
                _settingsWindow = null;
            };
            _settingsWindow.Activate();
        }
        else
        {
            _settingsWindow.Activate();
        }
        return Task.CompletedTask;
    }

    // ============================================================
    //  Extract (single) — Keka-style inline panel flow
    //
    // Decision tree on each single-archive extract request:
    //   1. Probe IsEncrypted (cheap, opens central dir only).
    //   2. If !forceDialog && !Settings_AskBeforeExtract:
    //        try silent extract with the stored default password (if any).
    //        On success — done. On WrongPassword — fall through to panel.
    //   3. Otherwise: open ExtractPanel inline, loop until success/cancel.
    //
    // forceDialog is set by OnDrop when the user held Ctrl or Alt during
    // the drop — that's the one-shot "actually I want to pick the
    // destination this time" override.
    // ============================================================
    /// <summary>
    /// Destination semantics for a shell extract verb. Drives which
    /// folder the archive unpacks into; see <see cref="ResolveDestForMode"/>.
    /// </summary>
    private enum ShellExtractMode
    {
        /// <summary>Settings-driven (drag &amp; drop / double-click default).</summary>
        Default,
        /// <summary>"여기에 풀기" — straight into the archive's parent folder.</summary>
        Here,
        /// <summary>"알아서 풀기" — in place if single-root, else into &lt;stem&gt;/.</summary>
        Smart,
        /// <summary>"&lt;stem&gt;\에 풀기" — always into &lt;stem&gt;/ (unique).</summary>
        Subfolder,
        /// <summary>"OtterZip으로 풀기..." — force the destination/options dialog.</summary>
        Dialog,
    }

    private Task ExtractAsync(string archivePath, bool forceDialog = false)
        => ExtractAsync(archivePath, ShellExtractMode.Default, forceDialog);

    private async Task ExtractAsync(string archivePath, ShellExtractMode mode, bool forceDialog = false)
    {
        DebugLog.Info("ExtractAsync begin: " + archivePath + " mode=" + mode + " forceDialog=" + forceDialog);
        // Probe for split-archive layout before touching any archive
        // reader — the Rust core's `open()` on a partial volume returns
        // a "Could not find EOCD" / mid-stream error that confuses
        // users. We classify and either route the multi-part set
        // through a dedicated handler or surface a friendly "v1.1
        // 예정" card for spanning forms we don't read yet.
        if (TryHandleSplitArchive(archivePath))
        {
            DebugLog.Info("ExtractAsync: handled as split archive, returning");
            return;
        }

        // Dialog mode always shows the destination/options panel.
        if (mode == ShellExtractMode.Dialog)
        {
            forceDialog = true;
        }

        bool isEncrypted;
        string suggestedDest;
        try
        {
            var probeStart = System.Diagnostics.Stopwatch.StartNew();
            isEncrypted = ProbeIsEncrypted(archivePath);
            DebugLog.Info("ExtractAsync: ProbeIsEncrypted took " + probeStart.ElapsedMilliseconds + "ms (encrypted=" + isEncrypted + ")");
            suggestedDest = ResolveDestForMode(archivePath, mode);
        }
        catch (Exception ex)
        {
            DebugLog.Info("ExtractAsync: probe failed: " + ex.Message);
            _jobQueue.ReportError(JobKind.Extract, Path.GetFileName(archivePath), ErrorMessages.Localize(ex));
            return;
        }

        bool askSetting = SettingsService.Get<bool>("Settings_AskBeforeExtract", false);
        if (!forceDialog && !askSetting
            && await TrySilentExtractAsync(archivePath, suggestedDest, isEncrypted).ConfigureAwait(true))
        {
            return;
        }

        await RunExtractPanelLoopAsync(archivePath, suggestedDest, isEncrypted, forceDialog).ConfigureAwait(true);
    }

    /// <summary>
    /// Resolve the extract destination for a shell verb mode. Bandizip
    /// parity (2026-06-17):
    ///   * <see cref="ShellExtractMode.Here"/> → parent folder, merge.
    ///   * <see cref="ShellExtractMode.Subfolder"/> → &lt;stem&gt;/ (unique).
    ///   * <see cref="ShellExtractMode.Smart"/> → single-root archives
    ///     unpack in place (the archive already wraps everything in one
    ///     folder); otherwise wrap in &lt;stem&gt;/ to avoid littering.
    ///   * <see cref="ShellExtractMode.Default"/> / <see cref="ShellExtractMode.Dialog"/>
    ///     → the settings-driven path (the dialog overrides anyway).
    /// </summary>
    private static string ResolveDestForMode(string archivePath, ShellExtractMode mode)
    {
        switch (mode)
        {
            case ShellExtractMode.Here:
                return ComputeExtractHerePath(archivePath);
            case ShellExtractMode.Subfolder:
                return ComputeSubfolderExtractPath(archivePath);
            case ShellExtractMode.Smart:
                return IsSingleRootFolderArchive(archivePath)
                    ? ComputeExtractHerePath(archivePath)
                    : ComputeSubfolderExtractPath(archivePath);
            default:
                return ResolveExtractDestination(archivePath);
        }
    }

    /// <summary>
    /// Forced "&lt;stem&gt;/" destination next to the archive, with a
    /// "(1)" / "(2)" suffix when that folder already exists. Independent
    /// of Settings_AlwaysExtractToSubfolder — the verb's whole point is
    /// the subfolder.
    /// </summary>
    private static string ComputeSubfolderExtractPath(string archivePath)
    {
        string baseDir = Path.GetDirectoryName(archivePath) ?? Directory.GetCurrentDirectory();
        string stem = StripArchiveExtension(Path.GetFileName(archivePath));
        return EnsureUniqueExtractDirectory(Path.Combine(baseDir, stem));
    }

    /// <summary>
    /// "알아서 풀기" layout probe — true when every entry lives under a
    /// single shared top-level folder (so a flat extract yields one tidy
    /// folder). Computed in managed code via the already-bound
    /// <c>ReadEntries</c> rather than the Rust <c>detect_root_layout</c>
    /// FFI (no extra P/Invoke surface; the core function stays for when
    /// extraction moves fully into the engine in v0.22). Any failure
    /// (encrypted headers, read error) defaults to <c>false</c> → safe
    /// subfolder wrap.
    /// </summary>
    private static bool IsSingleRootFolderArchive(string archivePath)
    {
        try
        {
            using var arc = Archive.Open(archivePath);
            string? root = null;
            foreach (var entry in arc.ReadEntries())
            {
                string p = entry.Path.Replace('\\', '/');
                int slash = p.IndexOf('/', StringComparison.Ordinal);
                if (slash <= 0)
                {
                    return false;   // a root-level file → flat layout
                }
                string top = p.Substring(0, slash);
                if (root is null)
                {
                    root = top;
                }
                else if (!string.Equals(root, top, StringComparison.Ordinal))
                {
                    return false;   // more than one top-level component
                }
            }
            return root is not null;
        }
        catch (Exception ex)
        {
            DebugLog.Info("IsSingleRootFolderArchive: probe failed, assuming flat: " + ex.Message);
            return false;
        }
    }

    /// <summary>
    /// Strip a single or compound archive extension for stem derivation.
    /// `foo.tar.gz` → `foo`, `bar.zip` → `bar`.
    /// </summary>
    private static string StripArchiveExtension(string fileName)
    {
        string[] compound = { ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz4", ".tar.lzma" };
        foreach (string suffix in compound)
        {
            if (fileName.EndsWith(suffix, StringComparison.OrdinalIgnoreCase))
            {
                return fileName.Substring(0, fileName.Length - suffix.Length);
            }
        }
        return Path.GetFileNameWithoutExtension(fileName);
    }

    /// <summary>
    /// Branch the extract flow based on multi-volume layout. Returns
    /// <c>true</c> when the caller should NOT continue with the normal
    /// single-volume open path.
    ///
    ///   * <see cref="SplitKind.Spanned"/> — surface a localized
    ///     "not supported" JobCard and return.
    ///   * <see cref="SplitKind.RawByteSplit"/> — enqueue a dedicated
    ///     concat-then-extract job that handles cleanup of the temp
    ///     file. The user sees a single card with combined progress.
    /// </summary>
    private bool TryHandleSplitArchive(string archivePath)
    {
        var split = SplitArchiveDetector.Probe(archivePath);
        switch (split.Kind)
        {
            case SplitKind.Spanned:
                EnqueueSpannedExtractJob(split);
                return true;
            case SplitKind.RawByteSplit:
                EnqueueRawByteSplitExtractJob(split);
                return true;
            default:
                return false;
        }
    }

    /// <summary>
    /// Attempt the silent (no-UI) extract path. Returns true when the
    /// caller should NOT fall through to the panel — either we succeeded,
    /// or we hit a fatal non-password error that the user already saw in
    /// the status bar. Returns false ONLY when the panel should take
    /// over (no stored password for an encrypted archive, or the stored
    /// password turned out to be wrong).
    /// </summary>
    private async Task<bool> TrySilentExtractAsync(string archivePath, string destination, bool isEncrypted)
    {
        DebugLog.Info("TrySilentExtractAsync: archivePath=" + archivePath + ", dest=" + destination + ", isEncrypted=" + isEncrypted);
        string? silentPw = null;
        if (isEncrypted)
        {
            silentPw = await ResolveStoredDefaultPasswordAsync(
                "Settings_DefaultPasswordOnExtract",
                _strings.GetString("Auth_ReasonExtract/Text")).ConfigureAwait(true);
            if (string.IsNullOrEmpty(silentPw))
            {
                return false; // need user input
            }
        }

        try
        {
            await PerformExtractAsync(archivePath, destination, silentPw).ConfigureAwait(true);
            return true;
        }
        catch (OtterzipException ex) when (ex.IsWrongPassword)
        {
            return false; // stored password didn't work — surface panel
        }
        catch (OperationCanceledException) { return true; }
        catch (Exception ex)
        {
            // Silent path fatal — surface as a JobCard so the user actually
            // sees the failure (the queue's own error path runs only when
            // the work delegate inside PerformExtractAsync was reached;
            // some failures throw before that).
            _jobQueue.ReportError(JobKind.Extract, Path.GetFileName(archivePath), ErrorMessages.Localize(ex));
            return true;
        }
    }

    /// <summary>
    /// Shows the inline ExtractPanel and loops on it until the user
    /// either submits a working extract or cancels. Wrong-password
    /// retries stay inside the same panel — no second dialog ever opens.
    ///
    /// <paramref name="forceDialog"/> doubles as <c>showDestination</c>:
    /// when the user held Ctrl/Alt during the drop they explicitly want
    /// to pick the destination, otherwise the panel starts in compact
    /// mode (password-only) and exposes an Advanced link to reveal the
    /// destination row on demand.
    /// </summary>
    private async Task RunExtractPanelLoopAsync(string archivePath, string suggestedDest, bool isEncrypted, bool forceDialog)
    {
        bool showDestination = forceDialog;
        SwitchView(AppView.Extract, extractHeight: PickExtractHeight(showDestination));
        // Pre-fill the password field with the stored default IF the
        // silent-extract path didn't already try it (i.e.
        // Settings_DefaultPasswordOnExtract is off). When the toggle is
        // on, any stored value was already attempted upstream — pre-
        // filling it again would just suggest a known-wrong value.
        // The Hello gate, if enabled, applies to actual auto-use; we
        // don't bypass it by pre-filling so we skip the convenience in
        // that case too.
        string? prefill = ResolveExtractPanelPrefill(isEncrypted);
        ExtractPanel.Configure(archivePath, suggestedDest, isEncrypted, showDestination, prefill);

        while (true)
        {
            var args = await PromptExtractPanelAsync().ConfigureAwait(true);
            if (args is null)
            {
                SwitchView(AppView.Idle);
                return;
            }

            string dest = args.ExtractHere
                ? ComputeExtractHerePath(archivePath)
                : args.Destination;

            try
            {
                SwitchView(AppView.Idle);
                await PerformExtractAsync(archivePath, dest, args.Password).ConfigureAwait(true);
                return;
            }
            catch (OtterzipException ex) when (ex.IsWrongPassword)
            {
                // Preserve the user's destination-visibility choice on
                // retry — if they expanded "Advanced" before submitting,
                // keep it expanded; otherwise stay compact.
                showDestination = showDestination || ExtractPanel.IsDestinationVisible;
                SwitchView(AppView.Extract, extractHeight: PickExtractHeight(showDestination));
                // Wrong-password retry: clear the field so the user
                // doesn't see the same wrong value they just submitted.
                ExtractPanel.Configure(archivePath, dest, needsPassword: true, showDestination: showDestination, prefillPassword: null);
                ExtractPanel.ShowError(_strings.GetString("Error_WrongPassword/Text"));
                // continue while-loop for retry
            }
            catch (OperationCanceledException) { SwitchView(AppView.Idle); return; }
            catch (Exception ex)
            {
                SwitchView(AppView.Idle);
                _jobQueue.ReportError(JobKind.Extract, Path.GetFileName(archivePath), ErrorMessages.Localize(ex));
                return;
            }
        }
    }

    /// <summary>
    /// Window height for the ExtractPanel in its current footprint.
    /// The outer StackPanel centers its content vertically, so the
    /// destination row can be revealed within the same window
    /// footprint as the compact (password-only) mode — no resize
    /// needed when "다른 폴더로…" expands.
    /// </summary>
    private int PickExtractWindowHeight()
        => PickExtractHeight(ExtractPanel.IsDestinationVisible);

    private static int PickExtractHeight(bool showDestination)
        => 360;

    /// <summary>
    /// Run one extract attempt — opens the archive, drives ExtractAllAsync
    /// with progress + cancel token plumbed through. Caller decides what
    /// to do with WrongPassword exceptions (silent path bails; panel loop
    /// re-prompts in the same panel).
    ///
    /// Rolls back partial output on WrongPassword: the Rust core opens
    /// the archive's central directory before verifying credentials, so
    /// the first entry's output file gets created and starts streaming
    /// before decryption rejects the password. Result without rollback
    /// is a fresh destination folder containing a 0-byte stub file. We
    /// remove the destination ONLY when this run created it; existing
    /// directories stay untouched (might hold unrelated user data).
    /// </summary>
    private async Task PerformExtractAsync(string archivePath, string destination, string? password)
    {
        bool destExistedBefore = Directory.Exists(destination);
        bool preserveMotw = SettingsService.Get<bool>("Settings_PreserveZoneIdentifier", true);

        // Phase 2: the actual archive work lives inside a JobQueue card.
        // The caller (RunExtractPanelLoopAsync or silent path) still needs
        // to know whether the run succeeded or hit WrongPassword, so we
        // bridge with a TaskCompletionSource — the work delegate fulfills
        // it before re-throwing for the queue's own state machine.
        var item = new JobItem(JobKind.Extract, Path.GetFileName(archivePath))
        {
            SourcePath = archivePath, // for Settings_DeleteArchiveAfterExtract
        };
        var done = new TaskCompletionSource<ExtractReport>(TaskCreationOptions.RunContinuationsAsynchronously);

        // Safety net: the queue can settle a job WITHOUT ever invoking the
        // work delegate (cancel while still Queued → WaitForSlotAsync bails),
        // in which case nothing would complete `done` and this await — and
        // any multi-archive loop above it — would hang forever (pre-1.0
        // review finding). The delegate's own TrySet* wins in every normal
        // path because it runs before the queue's settle event.
        void OnSettled(object? _, JobItem settled)
        {
            if (ReferenceEquals(settled, item))
            {
                done.TrySetCanceled();
            }
        }
        _jobQueue.JobSettled += OnSettled;
        try
        {
            _jobQueue.Submit(item, (ct, progress) =>
                RunInlineExtractWorkAsync(item, archivePath, destination, password,
                    preserveMotw, destExistedBefore, done, ct, progress));

            await done.Task.ConfigureAwait(true);
        }
        finally
        {
            _jobQueue.JobSettled -= OnSettled;
        }
    }

    /// <summary>
    /// Inline-extract work delegate body. Pulled out of PerformExtractAsync
    /// so the public method stays under the analyzer's 60-line cap; the
    /// flow is unchanged.
    /// </summary>
    private async Task RunInlineExtractWorkAsync(
        JobItem item, string archivePath, string destination, string? password,
        bool preserveMotw, bool destExistedBefore,
        TaskCompletionSource<ExtractReport> done,
        CancellationToken ct, IProgress<double> progress)
    {
        try
        {
            DebugLog.Info("RunInlineExtractWorkAsync: starting Archive.Open: " + archivePath);
            var openStart = System.Diagnostics.Stopwatch.StartNew();
            using var archive = string.IsNullOrEmpty(password)
                ? Archive.Open(archivePath)
                : Archive.OpenWithPassword(archivePath, password);
            DebugLog.Info("RunInlineExtractWorkAsync: Archive.Open done in " + openStart.ElapsedMilliseconds + "ms");
            var progressBridge = new Progress<ProgressUpdate>(p =>
                progress.Report(Math.Clamp(p.FractionComplete, 0.0, 1.0)));
            DebugLog.Info("RunInlineExtractWorkAsync: calling ExtractAllAsync, destination=" + destination);
            var extractStart = System.Diagnostics.Stopwatch.StartNew();
            var report = await archive
                .ExtractAllAsync(destination, ExtractDefaults.ResolveOverwrite(), progressBridge,
                    preserveZoneIdentifier: preserveMotw, cancellationToken: ct)
                .ConfigureAwait(false);
            DebugLog.Info("RunInlineExtractWorkAsync: ExtractAllAsync done in " + extractStart.ElapsedMilliseconds + "ms (entries=" + report.EntriesExtracted + ", bytes=" + report.BytesWritten + ")");
            var flattenStart = System.Diagnostics.Stopwatch.StartNew();
            TryFlattenRedundantWrapper(destination, destExistedBefore);
            DebugLog.Info("RunInlineExtractWorkAsync: TryFlattenRedundantWrapper done in " + flattenStart.ElapsedMilliseconds + "ms");

            string doneText = string.Format(CultureInfo.CurrentCulture,
                _strings.GetString("Main_StatusBarDoneFormat/Text"),
                report.EntriesExtracted,
                FormatByteSize(report.BytesWritten));
            var dispatchStart = System.Diagnostics.Stopwatch.StartNew();
            await DispatchUiAndWaitAsync(() =>
            {
                item.ResultPath = destination;
                item.Progress = 1.0;     // tripwire for JobQueue's monotonic guard
                item.StatusText = doneText;
            }).ConfigureAwait(false);
            DebugLog.Info("RunInlineExtractWorkAsync: UI dispatch done in " + dispatchStart.ElapsedMilliseconds + "ms");
            done.TrySetResult(report);
        }
        catch (OtterzipException ex) when (ex.IsWrongPassword)
        {
            RollbackPartialExtract(destination, destExistedBefore);
            done.TrySetException(ex);
            throw;
        }
        catch (OperationCanceledException)
        {
            RollbackPartialExtract(destination, destExistedBefore);
            done.TrySetCanceled(ct);
            throw;
        }
        catch (Exception ex)
        {
            done.TrySetException(ex);
            throw;
        }
    }

    /// <summary>
    /// Best-effort cleanup of a destination folder that this extract run
    /// created but failed to populate. Skips the cleanup if the folder
    /// pre-existed (it might contain unrelated user data we shouldn't
    /// touch) or if the deletion itself fails (file lock, permission).
    /// </summary>
    private static void RollbackPartialExtract(string destination, bool destExistedBefore)
    {
        // Free the in-process name reservation regardless — the output is
        // being abandoned, so a retry should get the clean name back.
        OutputNamer.Release(destination);
        if (destExistedBefore || !Directory.Exists(destination)) return;
        try
        {
            Directory.Delete(destination, recursive: true);
        }
        catch (IOException) { /* best effort */ }
        catch (UnauthorizedAccessException) { /* best effort */ }
    }

    /// <summary>
    /// Collapse the "wrapper folder of the same name" case after a
    /// successful extract. If we created <paramref name="destination"/>
    /// (i.e. <paramref name="destExistedBefore"/> is false) and the
    /// extract dumped everything into a single inner folder whose name
    /// matches <paramref name="destination"/>, hoist that inner folder's
    /// children up one level and remove the redundant inner folder.
    ///
    /// Example — user drops <c>TEST.zip</c> whose root is also
    /// <c>TEST/</c>:
    ///   Before: <c>parent/TEST/TEST/file1, parent/TEST/TEST/file2 …</c>
    ///   After:  <c>parent/TEST/file1, parent/TEST/file2 …</c>
    ///
    /// We deliberately stay conservative: we only flatten when the inner
    /// folder name matches the wrapper exactly. Archives whose root has
    /// a different meaningful name (e.g. <c>photo.zip</c> containing
    /// <c>Photos2024/</c>) are left alone so we don't lose information
    /// the archive author chose to encode.
    /// </summary>
    private static void TryFlattenRedundantWrapper(string destination, bool destExistedBefore)
    {
        if (destExistedBefore) return;            // user / existing folder — don't touch
        if (!Directory.Exists(destination)) return;
        try
        {
            var entries = Directory.GetFileSystemEntries(destination);
            if (entries.Length != 1) return;       // either empty or multi-root — leave as-is
            string inner = entries[0];
            if (!Directory.Exists(inner)) return;  // single file, not a folder — leave as-is
            if (!string.Equals(
                    Path.GetFileName(inner),
                    Path.GetFileName(destination),
                    StringComparison.OrdinalIgnoreCase))
            {
                return; // names differ — preserve the archive author's intent
            }

            // Two-step rename to dodge "inner shares parent's name" path
            // collisions: move inner out to a scratch directory next to
            // destination, hoist its children into destination, then
            // delete scratch. The scratch path is a sibling (not a child)
            // of destination so EnsureUniqueExtractDirectory's parent
            // ownership stays clean.
            string parent = Path.GetDirectoryName(destination) ?? destination;
            string scratch = Path.Combine(
                parent,
                ".otterzip-flatten-" + Guid.NewGuid().ToString("N"));
            Directory.Move(inner, scratch);
            foreach (var subPath in Directory.GetFileSystemEntries(scratch))
            {
                string name = Path.GetFileName(subPath);
                string target = Path.Combine(destination, name);
                if (Directory.Exists(subPath))
                {
                    Directory.Move(subPath, target);
                }
                else
                {
                    File.Move(subPath, target);
                }
            }
            Directory.Delete(scratch);
        }
        catch (IOException) { /* best effort — files already on disk */ }
        catch (UnauthorizedAccessException) { /* ditto */ }
    }

    // ============================================================
    //  Extract helpers (probe / dest resolution / view stack)
    // ============================================================

    /// <summary>
    /// Cheap up-front check — opens the archive to read its central
    /// directory and asks the core "do you need a password?". Header-
    /// encrypted formats (7z, RAR) where Open itself throws
    /// WrongPassword report as encrypted too.
    /// </summary>
    private static bool ProbeIsEncrypted(string archivePath)
    {
        try
        {
            using var probe = Archive.Open(archivePath);
            return probe.IsEncrypted();
        }
        catch (OtterzipException ex) when (ex.IsWrongPassword)
        {
            return true;
        }
        catch (Exception)
        {
            return false; // let downstream surface the real error
        }
    }

    /// <summary>
    /// Mirrors the destination logic ExtractManyAsync uses, kept in one
    /// helper so the silent path and the panel pre-fill always agree on
    /// where extracts land by default.
    /// </summary>
    private static string ResolveExtractDestination(string archivePath)
    {
        string extractLoc = SettingsService.Get<string>("Settings_ExtractLocation", "same");
        string customDir = SettingsService.Get<string>("Settings_ExtractLocationPath", "");
        bool useSubfolder = SettingsService.Get<bool>("Settings_AlwaysExtractToSubfolder", true);
        bool useCustom = string.Equals(extractLoc, "custom", StringComparison.Ordinal)
            && !string.IsNullOrWhiteSpace(customDir)
            && Directory.Exists(customDir);

        string baseDir = useCustom
            ? customDir
            : (Path.GetDirectoryName(archivePath) ?? Directory.GetCurrentDirectory());
        string stem = Path.GetFileNameWithoutExtension(archivePath);
        string dest = useSubfolder ? Path.Combine(baseDir, stem) : baseDir;
        // Only auto-rename the subfolder case; "extract directly into
        // baseDir" doesn't make sense to suffix.
        return useSubfolder ? EnsureUniqueExtractDirectory(dest) : dest;
    }

    /// <summary>
    /// "여기에 풀기" semantics — standard 7-Zip / WinRAR / Keka
    /// convention: drop every entry directly into the archive's parent
    /// folder, no subfolder. This is what makes the button meaningfully
    /// different from "추출" (which honours Settings_AlwaysExtractToSubfolder
    /// and Settings_ExtractLocation). No uniqueness suffix here either —
    /// the destination IS the user's chosen folder; files merging with
    /// existing siblings is the intended behaviour.
    /// </summary>
    private static string ComputeExtractHerePath(string archivePath)
    {
        return Path.GetDirectoryName(archivePath) ?? Directory.GetCurrentDirectory();
    }

    /// <summary>
    /// Mirror of <see cref="EnsureUniqueDestination"/> for extract
    /// targets — when "Downloads/foo" already exists, return
    /// "Downloads/foo (1)" / "(2)" / … so we never silently overwrite a
    /// folder full of someone's prior extract. Only applied to the auto-
    /// computed destination paths; user-typed targets are honoured as-is.
    /// </summary>
    private static string EnsureUniqueExtractDirectory(string path)
    {
        if (!Directory.Exists(path)) return path;
        string parent = Path.GetDirectoryName(path) ?? Directory.GetCurrentDirectory();
        string name = Path.GetFileName(path.TrimEnd(
            Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar));
        for (int i = 1; i < 10000; i++)
        {
            string candidate = Path.Combine(parent, string.Format(
                CultureInfo.InvariantCulture, "{0} ({1})", name, i));
            if (!Directory.Exists(candidate)) return candidate;
        }
        return Path.Combine(parent, string.Format(CultureInfo.InvariantCulture,
            "{0} ({1:yyyyMMddHHmmss})", name, DateTime.Now));
    }

    /// <summary>
    /// Toggle the body view + resize the window to fit. ConfigPanel is
    /// 460 tall; ExtractPanel stays at a single 360 height for both
    /// compact and full modes — the destination row reveals into the
    /// existing slack instead of triggering a resize.
    /// </summary>
    private void SwitchView(AppView view, int extractHeight = 360)
    {
        _ = extractHeight;   // legacy param — window no longer resizes for Extract
        _currentView = view;
        bool extract = view == AppView.Extract;
        // ConfigPanel stays visible underneath the extract overlay so
        // the canvas / cards remain perceptible behind the backdrop.
        ConfigPanel.Visibility = Visibility.Visible;
        // ExtractOverlay carries the dim backdrop + the floating card.
        ExtractOverlay.Visibility = extract ? Visibility.Visible : Visibility.Collapsed;
        // The window stays at its idle size — no shrinking, no growing.
    }

    /// <summary>
    /// Awaits the next user action on the inline ExtractPanel. Returns
    /// the submitted args, or null when the user cancelled. Caller is
    /// responsible for SwitchView before/after.
    /// </summary>
    private Task<UserControls.ExtractPanelSubmitArgs?> PromptExtractPanelAsync()
    {
        // The slot is single-occupancy. If a second extract arrives while a
        // panel is already up (drop B while A's panel shows, or a redirected
        // shell activation), dismiss the OLDER flow instead of orphaning its
        // TCS — otherwise A's loop awaits forever and A is silently never
        // extracted (pre-1.0 review finding).
        _pendingExtractTcs?.TrySetResult(null);
        _pendingExtractTcs = new TaskCompletionSource<UserControls.ExtractPanelSubmitArgs?>();
        return _pendingExtractTcs.Task;
    }

    private void OnExtractSubmitted(object? sender, UserControls.ExtractPanelSubmitArgs e)
    {
        _pendingExtractTcs?.TrySetResult(e);
        _pendingExtractTcs = null;
    }

    private void OnExtractDismissed(object? sender, EventArgs e)
    {
        _pendingExtractTcs?.TrySetResult(null);
        _pendingExtractTcs = null;
    }

    private Task ExtractManyAsync(IReadOnlyList<string> archives)
    {
        // Bulk extract: each archive becomes its own JobQueue card so the
        // user sees per-archive progress and per-archive cancel. No batch
        // password prompt — the queue UI handles errors per-card.
        string extractLoc = SettingsService.Get<string>("Settings_ExtractLocation", "same");
        string customDir = SettingsService.Get<string>("Settings_ExtractLocationPath", "");
        bool useSubfolder = SettingsService.Get<bool>("Settings_AlwaysExtractToSubfolder", true);
        bool preserveMotw = SettingsService.Get<bool>("Settings_PreserveZoneIdentifier", true);
        bool useCustom = string.Equals(extractLoc, "custom", StringComparison.Ordinal)
            && !string.IsNullOrWhiteSpace(customDir)
            && Directory.Exists(customDir);

        foreach (var archivePath in archives)
        {
            // Split-archive detection runs per-path so a mixed drop of
            // single-volume + spanned + raw-split archives each lands
            // on the right route.
            if (TryHandleSplitArchive(archivePath))
            {
                continue;
            }
            string baseDir = useCustom
                ? customDir
                : (Path.GetDirectoryName(archivePath) ?? Directory.GetCurrentDirectory());
            string stem = Path.GetFileNameWithoutExtension(archivePath);
            string dest = useSubfolder
                // Bulk plans all compute BEFORE any job writes — reserve the
                // directory name against the sibling jobs in this loop
                // (a.zip + a.7z both resolved to "…\a" and extracted into it
                // concurrently — pre-1.0 review finding).
                ? OutputNamer.ReserveUniqueDirectory(Path.Combine(baseDir, stem))
                : baseDir;
            EnqueueBulkExtractJob(archivePath, dest, preserveMotw);
        }
        return Task.CompletedTask;
    }

    /// <summary>
    /// Submit one archive's worth of extract work as a JobQueue card.
    /// Used by the bulk-extract path where there's no password prompt
    /// in the flow (archives that need a password surface as Error on
    /// their card, and the user can retry by single-dropping).
    /// </summary>
    private void EnqueueBulkExtractJob(string archivePath, string destination, bool preserveMotw)
    {
        var item = new JobItem(JobKind.Extract, Path.GetFileName(archivePath))
        {
            SourcePath = archivePath, // for Settings_DeleteArchiveAfterExtract
            // Reserved only in the sub-folder case; releasing a non-reserved
            // path is a harmless no-op (APP-C1).
            ReservedOutputPath = destination,
        };
        bool destExistedBefore = Directory.Exists(destination);

        _jobQueue.Submit(item, async (ct, progress) =>
        {
            try
            {
                using var archive = Archive.Open(archivePath);
                var progressBridge = new Progress<ProgressUpdate>(p =>
                    progress.Report(Math.Clamp(p.FractionComplete, 0.0, 1.0)));
                var report = await archive
                    .ExtractAllAsync(destination, ExtractDefaults.ResolveOverwrite(), progressBridge,
                        preserveZoneIdentifier: preserveMotw, cancellationToken: ct)
                    .ConfigureAwait(false);
                TryFlattenRedundantWrapper(destination, destExistedBefore);
                string doneText = string.Format(CultureInfo.CurrentCulture,
                    _strings.GetString("Main_StatusBarDoneFormat/Text"),
                    report.EntriesExtracted,
                    FormatByteSize(report.BytesWritten));
                await DispatchUiAndWaitAsync(() =>
                {
                    item.ResultPath = destination;
                    item.Progress = 1.0;
                    item.StatusText = doneText;
                }).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                RollbackPartialExtract(destination, destExistedBefore);
                throw;
            }
            catch (OtterzipException ex) when (ex.IsWrongPassword)
            {
                RollbackPartialExtract(destination, destExistedBefore);
                throw;
            }
        });
    }

    // ============================================================
    //  Raw byte-split (.zip.001..NNN / .7z.001..NNN) extract
    //
    // Tools like WinRAR's "split to volumes" without container
    // spanning emit a contiguous numbered sequence whose bytes
    // recombine into the original single-volume archive. v1.0 of
    // OtterZip cannot read these volumes natively (the Rust `zip`
    // crate operates on a single file handle), so we recombine
    // into a temp file up-front and then route the result through
    // the regular extract flow. The temp file is removed on every
    // exit path — success, cancel, or error.
    //
    // Encryption is not surfaced through an interactive password
    // panel on this route; if the archive turns out to be
    // encrypted, the silent-extract path tries Settings_
    // DefaultPasswordOnExtract (when ON), otherwise the job lands
    // on the Error card with a wrong-password message. Interactive
    // password prompts for raw byte-split sets are deferred to v1.1
    // — by then the native multi-volume reader should subsume this
    // helper entirely.
    // ============================================================
    // ============================================================
    //  Spanned ZIP / 7z extract (.z01..zN + .zip / .7z.001..NNN)
    //
    // v1.0 (ABI v8): supported for ZIP via Archive.OpenMulti — the
    // native side stitches all volumes into a virtual byte stream so
    // the standard extract pipeline can walk them as one archive. 7z
    // container spanning still falls through with UnsupportedFormat,
    // which we catch here and surface as the localized "v1.1 예정"
    // card.
    // ============================================================
    private void EnqueueSpannedExtractJob(SplitArchiveDetector.DetectionResult split)
    {
        var item = new JobItem(JobKind.Extract, Path.GetFileName(split.EntryPath))
        {
            SourcePath = split.EntryPath,
        };
        string destination = ResolveExtractDestination(split.EntryPath);
        bool destExistedBefore = Directory.Exists(destination);
        bool preserveMotw = SettingsService.Get<bool>("Settings_PreserveZoneIdentifier", true);

        _jobQueue.Submit(item, (ct, progress) =>
            RunSpannedExtractWorkAsync(
                item, split, destination, destExistedBefore, preserveMotw, ct, progress));
    }

    /// <summary>
    /// Work delegate for the spanned (container-aware) extract job.
    /// Opens every volume via <see cref="Archive.OpenMulti"/> and runs
    /// the standard extract pipeline. If the native side reports
    /// UnsupportedFormat (e.g. 7z spanning — not yet implemented), the
    /// job surfaces a localized "v1.1 예정" message instead of leaking
    /// the raw native error.
    /// </summary>
    private async Task RunSpannedExtractWorkAsync(
        JobItem item, SplitArchiveDetector.DetectionResult split,
        string destination, bool destExistedBefore, bool preserveMotw,
        CancellationToken ct, IProgress<double> progress)
    {
        try
        {
            using var archive = Archive.OpenMulti(split.Volumes);
            var progressBridge = new Progress<ProgressUpdate>(p =>
                progress.Report(Math.Clamp(p.FractionComplete, 0.0, 1.0)));
            var report = await archive
                .ExtractAllAsync(destination, ExtractDefaults.ResolveOverwrite(), progressBridge,
                    preserveZoneIdentifier: preserveMotw, cancellationToken: ct)
                .ConfigureAwait(false);
            TryFlattenRedundantWrapper(destination, destExistedBefore);

            string doneText = string.Format(CultureInfo.CurrentCulture,
                _strings.GetString("Main_StatusBarDoneFormat/Text"),
                report.EntriesExtracted,
                FormatByteSize(report.BytesWritten));
            await DispatchUiAndWaitAsync(() =>
            {
                item.ResultPath = destination;
                item.Progress = 1.0;
                item.StatusText = doneText;
            }).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            RollbackPartialExtract(destination, destExistedBefore);
            throw;
        }
        catch (OtterzipException ex)
            when (ex.Message.Contains("UnsupportedFormat", StringComparison.OrdinalIgnoreCase)
               || ex.Message.Contains("not supported", StringComparison.OrdinalIgnoreCase))
        {
            // 7z spanning lands here. Convert to the user-facing
            // "v1.1 예정" card so the message is intelligible regardless
            // of locale or upstream wording.
            RollbackPartialExtract(destination, destExistedBefore);
            string msg = string.Format(CultureInfo.CurrentCulture,
                _strings.GetString("Split_NotSupportedFormat/Text"),
                split.Volumes.Count);
            // AlreadyLocalized: msg is a finished localized string. Without this
            // sentinel, JobQueue.RunAsync → ErrorMessages.Localize would map the
            // unknown code to the generic "Error_ArchiveFailed" headline and the
            // "spanned 7z — v1.1 예정" explanation would be lost.
            throw new OtterzipException(OtterzipErrorCodes.AlreadyLocalized, msg);
        }
    }

    private void EnqueueRawByteSplitExtractJob(SplitArchiveDetector.DetectionResult split)
    {
        var item = new JobItem(JobKind.Extract, Path.GetFileName(split.EntryPath))
        {
            // Reveal-on-completion shows the file the user actually
            // dropped, not the temp recombined archive.
            SourcePath = split.EntryPath,
        };

        string tempPath = Path.Combine(
            Path.GetTempPath(),
            "otterzip-join-" + Guid.NewGuid().ToString("N") + Path.GetExtension(split.Stem));
        string destination = ResolveExtractDestination(split.EntryPath);
        bool destExistedBefore = Directory.Exists(destination);
        bool preserveMotw = SettingsService.Get<bool>("Settings_PreserveZoneIdentifier", true);

        _jobQueue.Submit(item, (ct, progress) =>
            RunRawByteSplitExtractWorkAsync(
                item, split, tempPath, destination, destExistedBefore, preserveMotw, ct, progress));
    }

    /// <summary>
    /// Work delegate for the raw byte-split job. Two phases share one
    /// progress bar: concat-to-temp (40%) then real extract (60%). The
    /// temp archive is always removed in <c>finally</c> so a cancel or
    /// crash never leaves a multi-GB stub in %TEMP%.
    /// </summary>
    private async Task RunRawByteSplitExtractWorkAsync(
        JobItem item, SplitArchiveDetector.DetectionResult split,
        string tempPath, string destination, bool destExistedBefore, bool preserveMotw,
        CancellationToken ct, IProgress<double> progress)
    {
        string joiningFormat = _strings.GetString("Split_StatusJoining/Text");
        try
        {
            await ConcatVolumesAsync(
                split.Volumes, tempPath, item, joiningFormat,
                fraction => progress.Report(fraction * 0.4), ct).ConfigureAwait(false);

            using var archive = Archive.Open(tempPath);
            var progressBridge = new Progress<ProgressUpdate>(p =>
                progress.Report(0.4 + Math.Clamp(p.FractionComplete, 0.0, 1.0) * 0.6));
            var report = await archive
                .ExtractAllAsync(destination, ExtractDefaults.ResolveOverwrite(), progressBridge,
                    preserveZoneIdentifier: preserveMotw, cancellationToken: ct)
                .ConfigureAwait(false);
            TryFlattenRedundantWrapper(destination, destExistedBefore);

            string doneText = string.Format(CultureInfo.CurrentCulture,
                _strings.GetString("Main_StatusBarDoneFormat/Text"),
                report.EntriesExtracted,
                FormatByteSize(report.BytesWritten));
            await DispatchUiAndWaitAsync(() =>
            {
                item.ResultPath = destination;
                item.Progress = 1.0;
                item.StatusText = doneText;
            }).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            RollbackPartialExtract(destination, destExistedBefore);
            throw;
        }
        catch (OtterzipException ex) when (ex.IsWrongPassword)
        {
            RollbackPartialExtract(destination, destExistedBefore);
            throw;
        }
        finally
        {
            TryDeleteTempJoinedArchive(tempPath);
        }
    }

    /// <summary>
    /// Streams every volume in <paramref name="volumes"/> into a single
    /// temp file at <paramref name="destPath"/>. The status caption is
    /// updated as each volume rolls over; the progress fraction is
    /// reported continuously so the JobCard's bar moves smoothly across
    /// volume boundaries.
    /// </summary>
    private async Task ConcatVolumesAsync(
        IReadOnlyList<string> volumes, string destPath, JobItem item,
        string joiningFormat, Action<double> reportFraction, CancellationToken ct)
    {
        long total = 0;
        for (int i = 0; i < volumes.Count; i++)
        {
            total += new FileInfo(volumes[i]).Length;
        }
        if (total <= 0) total = 1; // avoid /0 on empty edge case

        long bytesDone = 0;
        const int bufferSize = 1 << 20; // 1 MiB — matches the Rust progress tick.
        byte[] buffer = new byte[bufferSize];

        using (var outStream = new FileStream(destPath, FileMode.CreateNew, FileAccess.Write,
            FileShare.None, bufferSize, FileOptions.SequentialScan))
        {
            for (int i = 0; i < volumes.Count; i++)
            {
                ct.ThrowIfCancellationRequested();
                string caption = string.Format(CultureInfo.CurrentCulture,
                    joiningFormat, i + 1, volumes.Count);
                // Phase label only — JobQueue's progress reporter
                // glues the percent suffix on. Writing to StatusText
                // directly here used to race with that reporter and
                // produced a 30 Hz "1/3 병합 중" ↔ "42%" flicker.
                DispatcherQueue.TryEnqueue(() => item.StatusLabel = caption);

                using var inStream = new FileStream(volumes[i], FileMode.Open, FileAccess.Read,
                    FileShare.Read, bufferSize, FileOptions.SequentialScan);
                int read;
                while ((read = await inStream.ReadAsync(buffer.AsMemory(0, bufferSize), ct).ConfigureAwait(false)) > 0)
                {
                    await outStream.WriteAsync(buffer.AsMemory(0, read), ct).ConfigureAwait(false);
                    bytesDone += read;
                    reportFraction((double)bytesDone / total);
                }
            }
            await outStream.FlushAsync(ct).ConfigureAwait(false);
        }
    }

    private static void TryDeleteTempJoinedArchive(string path)
    {
        try
        {
            if (File.Exists(path))
            {
                File.Delete(path);
            }
        }
        catch (IOException) { /* file lock — leave for the OS temp cleanup */ }
        catch (UnauthorizedAccessException) { /* ditto */ }
    }

    // ExtractWithPasswordRetryAsync was retired with the inline-panel
    // refactor — PerformExtractAsync + RunExtractPanelLoopAsync subsume
    // its responsibilities (run extract / catch WrongPassword / re-prompt
    // in the same panel rather than a second modal).
    //
    // PromptPasswordAsync stays — Compress flow still uses it for the
    // Settings_AlwaysPromptPassword case. A follow-up sprint can move
    // that to an inline panel too.

    private async Task<string?> PromptPasswordAsync(string archiveName)
    {
        var passwordBox = new PasswordBox
        {
            PlaceholderText = _strings.GetString("PasswordDialog_Placeholder/Text"),
            Width = 280,
        };
        var dialog = new ContentDialog
        {
            Title = string.Format(CultureInfo.CurrentCulture,
                _strings.GetString("PasswordDialog_TitleFormat/Text"),
                archiveName),
            Content = passwordBox,
            PrimaryButtonText = _strings.GetString("PasswordDialog_OK/Text"),
            CloseButtonText = _strings.GetString("PasswordDialog_Cancel/Text"),
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot,
        };
        var result = await dialog.ShowAsync();
        return result == ContentDialogResult.Primary ? passwordBox.Password : null;
    }

    // ============================================================
    //  Compress — Phase 1: routed through JobQueue
    //
    // Each request resolves its password / plan up front, then drops one
    // or more JobItem entries on the queue. The queue owns concurrency,
    // cancellation, and progress; this method returns as soon as the
    // submissions are recorded so the caller (drag/drop handler) is
    // never blocked by long-running work.
    //
    // When "따로따로 압축하기" (Settings_CompressSeparately) is on with
    // N>1 sources, each source becomes its own card in the queue. This
    // also gives the user per-archive cancel / open-folder UX.
    // ============================================================
    private async Task CompressAsync(IReadOnlyList<string> sources)
    {
        string? compressPassword = await ResolveCompressPasswordAsync().ConfigureAwait(true);
        if (compressPassword is null && SettingsService.Get<bool>("Settings_AlwaysPromptPassword", false))
        {
            // Always-prompt is on but user backed out — abort the whole job.
            return;
        }

        bool separately = SettingsService.Get<bool>("Settings_CompressSeparately", false);
        if (separately && sources.Count > 1)
        {
            foreach (var src in sources)
            {
                EnqueueCompressJob(new[] { src }, compressPassword);
            }
        }
        else
        {
            EnqueueCompressJob(sources, compressPassword);
        }
    }

    /// <summary>
    /// Build the compress plan + create a JobItem + submit it to the
    /// queue. The work delegate captures everything the actual
    /// compress run needs; the queue handles state transitions and
    /// surfaces them to the floating card.
    /// </summary>
    private void EnqueueCompressJob(IReadOnlyList<string> sources, string? password)
    {
        var plan = PlanCompress(sources);
        var item = new JobItem(JobKind.Compress, Path.GetFileName(plan.Destination))
        {
            ReservedOutputPath = plan.Destination, // released by the queue on settle (APP-C1)
        };
        _jobQueue.Submit(item, (ct, progress) =>
            RunCompressWorkAsync(item, plan, sources, password, ct, progress));
    }

    /// <summary>
    /// Build the rich-progress reporter the compress work delegate
    /// hands to <c>RunCompressAsync</c>. Extracted from the body so
    /// the caller stays under the analyzer's 60-line cap and so the
    /// per-entry byte-progress wiring (ABI v9, second progress bar
    /// for the streaming compress path) sits in one place.
    /// </summary>
    private Progress<ProgressUpdate> BuildCompressRichProgress(
        JobItem item, IProgress<double> overallProgress, string compressingText)
    {
        return new Progress<ProgressUpdate>(p =>
        {
            double frac = p.FractionComplete;
            if (frac > 0)
            {
                overallProgress.Report(Math.Clamp(frac, 0.0, 1.0));
            }
            if (p.Phase == ProgressPhase.Writing)
            {
                // Phase label only — JobQueue's progress reporter
                // glues the percent suffix on. Writing StatusText
                // directly here used to race with that reporter and
                // flicker 30 Hz between "압축 중…" and "42%".
                DispatcherQueue.TryEnqueue(() => item.StatusLabel = compressingText);
            }
            // ABI v9 — per-entry byte progress (streaming compress
            // path only). When the native side fills these fields we
            // surface them as a second progress bar so the user can
            // tell a 3 GB single-file deflate from a genuinely stuck
            // worker. Outside the streaming path the totals stay 0;
            // we then collapse the bar so the JobCard's vertical
            // height stays stable on chunked-rayon workloads.
            bool hasEntryProgress = p.CurrentEntryBytesTotal > 0;
            double entryFrac = p.CurrentEntryFraction;
            // Surface the current entry filename when the native side
            // supplies it. The writer fills `CurrentEntry` on
            // entry-complete ticks (small-pipeline ordered drain) and
            // every 1 MiB tick (large-streaming path), then leaves it
            // null on the in-flight forecast ticks — we preserve the
            // last non-null name across nulls so the row doesn't
            // strobe between visible and collapsed at 30 Hz.
            string? entryName = p.CurrentEntry;
            DispatcherQueue.TryEnqueue(() =>
            {
                item.CurrentEntryProgress = entryFrac;
                item.CurrentEntryProgressVisible = hasEntryProgress;
                if (!string.IsNullOrEmpty(entryName))
                {
                    item.CurrentEntryName = entryName;
                }
            });
        });
    }

    /// <summary>
    /// Compress work delegate body. Extracted from EnqueueCompressJob so
    /// the public method stays under the analyzer's 60-line cap.
    /// </summary>
    private async Task RunCompressWorkAsync(
        JobItem item, CompressEngine.CompressPlan plan, IReadOnlyList<string> sources,
        string? password, CancellationToken ct, IProgress<double> progress)
    {
        // ABI v7/v9: the native side reports byte/entry counts per
        // file and (since v9) per-entry byte progress for the
        // streaming compress path. Helper composes the callback.
        string compressingText = _strings.GetString("Job_StatusCompressing/Text");
        var richProgress = BuildCompressRichProgress(item, progress, compressingText);

        // Cancel feedback: caption flips to "취소 중…" the moment the
        // user clicks X. Native side observes the CT on the next entry
        // boundary and throws OperationCanceledException; the catch
        // below cleans up the partial archive.
        string cancellingText = _strings.GetString("Job_StatusCancelling/Text");
        using var cancellingReg = ct.Register(() =>
            // Phase label only. After this fires no further percent
            // ticks arrive (the work delegate races against cancel),
            // so the user sees the last "압축 중… 42%" composed line
            // for a moment until MarkCancelled flips StatusText to
            // the final "취소됨" terminal message.
            DispatcherQueue.TryEnqueue(() => item.StatusLabel = cancellingText));

        try
        {
            var workTimer = System.Diagnostics.Stopwatch.StartNew();
            var report = await RunCompressAsync(plan, sources, password, richProgress, ct).ConfigureAwait(false);
            workTimer.Stop();
            ulong originalBytes = OperationSummary.TotalInputBytes(sources);
            string summary = OperationSummary.Compress(originalBytes, report.BytesWritten, workTimer.Elapsed);
            // Split output is a .NNN set with no single archive to re-open, so
            // skip CRC verify there (the slices are byte-faithful — see the
            // core split round-trip tests); reveal/open targets the .001.
            if (plan.VolumeSizeBytes == 0)
            {
                await MaybeVerifyAsync(plan.Destination, ct, password).ConfigureAwait(false);
            }
            MaybeRecycleSources(sources);
            string resultPath = plan.VolumeSizeBytes > 0 ? plan.Destination + ".001" : plan.Destination;
            await DispatchUiAndWaitAsync(() =>
            {
                item.ResultPath = resultPath;
                item.Progress = 1.0;
                item.StatusText = summary;
            }).ConfigureAwait(false);
        }
        catch
        {
            // The output is unusable on ANY failure (cancel, disk full, IO
            // error) — a truncated archive left behind looks real. Sweep the
            // base file and, in split mode, the .001..NNN segment set.
            TryDeletePartialArchive(plan.Destination, plan.VolumeSizeBytes);
            throw;
        }
    }

    private static void TryDeletePartialArchive(string path, ulong volumeSizeBytes = 0)
        => CompressEngine.TryDeletePartialArchive(path, volumeSizeBytes);

    /// <summary>
    /// Marshal <paramref name="action"/> onto the window's DispatcherQueue
    /// and await its completion. Used by JobQueue work delegates to apply
    /// final-state updates before the queue transitions the job to Done.
    /// </summary>
    private Task DispatchUiAndWaitAsync(Action action)
    {
        var tcs = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        bool ok = DispatcherQueue.TryEnqueue(() =>
        {
            try { action(); }
            finally { tcs.TrySetResult(); }
        });
        if (!ok)
        {
            // Dispatcher refused (shutting down?) — fall back to inline so
            // the work delegate doesn't hang the queue's RunAsync.
            try { action(); }
            finally { tcs.TrySetResult(); }
        }
        return tcs.Task;
    }

    /// <summary>
    /// Decide which password (if any) the upcoming compress job should use.
    /// Precedence: ConfigPanel input → DefaultPassword setting → optional
    /// always-prompt dialog → none. Returns null when no password applies
    /// or when the user cancelled the always-prompt.
    /// </summary>
    private Task<string?> ResolveCompressPasswordAsync()
    {
        // Precedence + Hello gating now live in
        // Services/PasswordResolver so the headless ProgressDialog
        // flow can run the same logic without reaching into
        // MainWindow's ConfigPanel.
        return PasswordResolver.ResolveCompressAsync(
            panelPassword: ConfigPanel.Password,
            authReason: _strings.GetString("Auth_ReasonCompress/Text"),
            promptArchiveLabel: _strings.GetString("PasswordDialog_PlaceholderArchiveLabel/Text"),
            prompter: PromptPasswordAsync);
    }

    /// <summary>
    /// Thin wrapper preserving the existing call sites
    /// (`ResolveExtractPanelPrefill(isEncrypted)`) while the body now
    /// lives in <see cref="PasswordResolver.ResolveExtractPanelPrefill"/>.
    /// </summary>
    private static string? ResolveExtractPanelPrefill(bool isEncrypted)
        => PasswordResolver.ResolveExtractPanelPrefill(isEncrypted);

    private static Task<string?> ResolveStoredDefaultPasswordAsync(string toggleKey, string authReason)
        => PasswordResolver.ResolveStoredDefaultAsync(toggleKey, authReason);

    /// <summary>
    /// Honor the <c>Settings_DeleteSourceAfterCompress</c> toggle: every
    /// source path that survived the compress moves to the Recycle Bin.
    /// Runs only after a successful compress + verify (caller-gated) so a
    /// failed build never destroys the user's input.
    /// </summary>
    private static void MaybeRecycleSources(IReadOnlyList<string> sources)
        => CompressEngine.MaybeRecycleSources(sources);

    private static Task MaybeVerifyAsync(string archivePath, CancellationToken ct, string? password = null)
        => CompressEngine.MaybeVerifyAsync(archivePath, ct, password);

    private CompressEngine.CompressPlan PlanCompress(IReadOnlyList<string> sources)
    {
        var plan = CompressEngine.BuildPlan(sources, ConfigPanel.SelectedFormat);
        // The "압축 옵션" flyout exposes preset volume sizes; thread it into the
        // plan (0 / negative-custom → a single archive).
        long split = ConfigPanel.SplitSizeBytes;
        return split > 0 ? plan with { VolumeSizeBytes = (ulong)split } : plan;
    }

    private static Task<ArchiveBuildReport> RunCompressAsync(
        CompressEngine.CompressPlan plan,
        IReadOnlyList<string> sources,
        string? password,
        IProgress<ProgressUpdate>? progress,
        CancellationToken ct)
        => CompressEngine.RunAsync(plan, sources, password, progress, ct);

    // ============================================================
    //  Formatting helpers
    // ============================================================
    private static string FormatByteSize(ulong bytes)
    {
        const ulong KB = 1024;
        const ulong MB = KB * 1024;
        const ulong GB = MB * 1024;
        if (bytes >= GB)
        {
            return string.Format(CultureInfo.CurrentCulture, "{0:0.##} GB", bytes / (double)GB);
        }
        if (bytes >= MB)
        {
            return string.Format(CultureInfo.CurrentCulture, "{0:0.##} MB", bytes / (double)MB);
        }
        if (bytes >= KB)
        {
            return string.Format(CultureInfo.CurrentCulture, "{0:0.##} KB", bytes / (double)KB);
        }
        return string.Format(CultureInfo.CurrentCulture, "{0} B", bytes);
    }
}
