using System;
using System.Collections.Generic;
using System.Linq;
using Microsoft.UI.Xaml;
using OtterZip.App.Services;
using OtterZip.Interop;
using Windows.Globalization;

namespace OtterZip.App;

public partial class App : Application
{
    private Window? _mainWindow;

    /// <summary>
    /// The currently active main window, or <see langword="null"/> before
    /// <see cref="OnLaunched"/> fires. Sub-dialogs use this to root file
    /// pickers — picker WinRT APIs need a window handle.
    /// </summary>
    public static Window? HostWindow { get; private set; }

    public App()
    {
        InitializeComponent();
        // Surface unhandled XAML exceptions to the debugger output and
        // (when running attached) the Output window — without this the
        // WinAppSDK runtime swallows the inner Exception and the
        // breakpoint stops at `UnhandledExceptionEventArgs` with no
        // message, which is exactly the failure mode that hid the
        // SetIcon path-resolution bug. Marking Handled=true keeps the
        // process alive for any non-fatal regressions; truly fatal
        // failures (StackOverflow / AccessViolation) bypass this anyway.
        UnhandledException += (_, e) =>
        {
            System.Diagnostics.Debug.WriteLine(
                "[OtterZip] UnhandledException: " + e.Message + "\n" + e.Exception);
            // Sentry auto-hooks AppDomain / unobserved-task exceptions but NOT
            // the WinUI UI-thread Application.UnhandledException, so forward it
            // here (no-op until the user opts in). Capture before Handled=true.
            OtterzipTelemetry.CaptureException(e.Exception);
            e.Handled = true;
        };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        InitializeAppServices();

        // Sprint 5: shell extension routes context-menu verbs through
        // `OtterZip.exe --invoke <verb> --files "..."`. We parse the
        // command line up front so the invoking workflow can complete
        // without showing the main window when appropriate.
        var invokeRequest = ParseInvokeArgs(Environment.GetCommandLineArgs());

        // Headless shell verbs skip MainWindow entirely and route to a
        // standalone modal (a small dialog instead of the full app surface).
        if (TryRouteHeadlessInvoke(invokeRequest))
        {
            return;
        }

        // "Open with" / default-app activation: Explorer launches us with
        // the archive file(s) through the WinAppSDK activation args (NOT
        // argv), so a plain `OnLaunched` would otherwise drop them and show
        // an empty window. Route them through the same pipeline as dragged
        // files (extract archives / compress other inputs).
        var openFiles = TryGetFileActivationPaths();

        // Double-click / "Open with" on archive(s) → extract per the
        // Settings_DoubleClickAction preference (dialog / here / smart). May
        // rewrite invokeRequest to a quick-extract verb for the MainWindow.
        if (invokeRequest is null && TryRouteArchiveActivation(openFiles, ref invokeRequest))
        {
            return;
        }

        _mainWindow = new MainWindow();
        HostWindow = _mainWindow;
        if (invokeRequest is not null)
        {
            ((MainWindow)_mainWindow).PreloadInvoke(invokeRequest);
        }
        else if (openFiles.Count > 0)
        {
            ((MainWindow)_mainWindow).PreloadFiles(openFiles);
        }
        _mainWindow.Activate();

        // First-run onboarding — only on a plain launch (not a shell verb
        // or "open with" file activation, where the user is mid-task). No-op
        // once completed/skipped.
        if (invokeRequest is null && openFiles.Count == 0)
        {
            OnboardingService.ShowIfFirstRun(_mainWindow.DispatcherQueue);
        }
    }

    /// <summary>
    /// Headless shell verbs skip MainWindow and route to a standalone modal
    /// (each owns its own JobQueue + dispatcher; the process exits when the
    /// last window closes). Returns <c>true</c> when it took over the launch.
    ///   * compress-zip / compress-7z  → ProgressDialog (quick, no form)
    ///   * compress (no QuickFormat)    → CompressOptionsDialog
    ///   * extract-dialog               → ExtractOptionsDialog
    /// </summary>
    private bool TryRouteHeadlessInvoke(InvokeRequest? invokeRequest)
    {
        if (invokeRequest is not { IsHeadless: true })
        {
            return false;
        }
        Window dialog = invokeRequest switch
        {
            { Verb: "extract-dialog" } =>
                new OtterZip.App.Modals.ExtractOptionsDialog(invokeRequest),
            { Verb: "compress", QuickFormat: null } =>
                new OtterZip.App.Modals.CompressOptionsDialog(invokeRequest),
            _ => new OtterZip.App.Modals.ProgressDialog(invokeRequest),
        };
        HostWindow = dialog;
        dialog.Activate();
        return true;
    }

    /// <summary>
    /// Decide how a double-click / "Open with" on archive(s) opens, per the
    /// <c>Settings_DoubleClickAction</c> preference:
    /// <list type="bullet">
    ///   <item>0 (default) — the focused <see cref="OtterZip.App.Modals.ExtractOptionsDialog"/>
    ///   (handles one or many archives sequentially); takes over the launch.</item>
    ///   <item>1 / 2 — quick "extract here" / "smart extract": rewrites
    ///   <paramref name="request"/> to the matching verb and falls through to
    ///   the MainWindow extract path.</item>
    /// </list>
    /// Returns <c>true</c> only when it fully handled the launch (the dialog).
    /// Non-archive inputs leave <paramref name="request"/> untouched.
    /// </summary>
    private bool TryRouteArchiveActivation(IReadOnlyList<string> openFiles, ref InvokeRequest? request)
    {
        if (openFiles.Count == 0 || !openFiles.All(MainWindow.IsKnownArchive))
        {
            return false;
        }
        int action = SettingsService.Get<int>("Settings_DoubleClickAction", 0);
        if (action == 0)
        {
            var extractRequest = new InvokeRequest(
                "extract-dialog", openFiles, QuickFormat: null, IsHeadless: true);
            Window extractDialog = new OtterZip.App.Modals.ExtractOptionsDialog(extractRequest);
            HostWindow = extractDialog;
            extractDialog.Activate();
            return true;
        }
        // Quick extract: route through the MainWindow shell-verb path (1=here,
        // 2=smart). Not headless — MainWindow's verb dispatch performs it.
        string verb = action == 2 ? "extract-smart" : "extract-here";
        request = new InvokeRequest(verb, openFiles, QuickFormat: null, IsHeadless: false);
        return false;
    }

    /// <summary>
    /// Paths of files opened via a file association / "Open with", or empty
    /// when this launch wasn't a file activation. Windows delivers the
    /// opened files through <c>AppInstance.GetActivatedEventArgs</c> (the
    /// command line stays empty), so reading argv alone would miss them.
    /// </summary>
    private static IReadOnlyList<string> TryGetFileActivationPaths()
    {
        try
        {
            var activated = Microsoft.Windows.AppLifecycle.AppInstance.GetCurrent()?.GetActivatedEventArgs();
            if (activated?.Kind == Microsoft.Windows.AppLifecycle.ExtendedActivationKind.File
                && activated.Data is Windows.ApplicationModel.Activation.IFileActivatedEventArgs fileArgs)
            {
                return fileArgs.Files
                    .Select(item => item.Path)
                    .Where(p => !string.IsNullOrEmpty(p))
                    .ToList();
            }
        }
        catch (Exception)
        {
            // Activation introspection is best-effort — fall back to a
            // normal empty-window launch rather than crashing.
        }
        return Array.Empty<string>();
    }

    /// <summary>
    /// One-time process initialization shared by every launch path
    /// (MainWindow + the headless dialogs). Extracted from
    /// <see cref="OnLaunched"/> to keep that method under the MA0051
    /// 60-line cap.
    /// </summary>
    private static void InitializeAppServices()
    {
        OtterzipLibrary.Initialize();
        // Phase 7 (PR-7F): telemetry pipeline. Honors the persisted user
        // preference first; OTTERZIP_TELEMETRY=1 env var inside Initialize()
        // is a fallback for CI / power users. Default is OFF.
        // Anonymous per-install id (random GUID, generated once) so a user can
        // request deletion of their crash reports — see PRIVACY.md. Set before
        // Initialize so it tags every event from the first one.
        OtterzipTelemetry.SetInstallId(GetOrCreateInstallId());
        OtterzipTelemetry.Initialize();
        if (SettingsService.Get<bool>("Settings_TelemetryEnabled", false))
        {
            OtterzipTelemetry.SetUserOptIn(true);
        }

        // Phase 7 (PR-7C): one-time credential migration from
        // SettingsService LocalSettings into Windows PasswordVault.
        // Idempotent on subsequent launches.
        CredentialStore.MigrateFromSettingsServiceOnce();

        // Phase 6+: honor saved language preference before any UI loads
        // resources. Empty string keeps system default, "ko" / "en" override.
        // PrimaryLanguageOverride is package-identity gated and throws in
        // unpackaged dev runs — ApplyLanguageOverride swallows that.
        ApplyLanguageOverride();

        // Mirror shell settings + localized verb strings into the package
        // LocalState so the shell handler's menu-build path reads them with
        // pure Win32 (no AppX activation context) — fixes the cold-boot
        // first-right-click verb miss. After the language override so the
        // cached strings match the active locale. Best-effort; never throws.
        ShellMenuCache.Refresh();

        // First launch after an install/update: broadcast SHCNE_ASSOCCHANGED
        // once so Explorer reloads the freshly (re)registered context-menu
        // verbs without waiting for a reboot. One-shot per version;
        // best-effort. See ShellAssociationNotifier for scope + limits.
        ShellAssociationNotifier.NotifyIfVersionChanged();
    }

    /// <summary>
    /// Get (or create on first run) the anonymous install id — a random GUID
    /// persisted in LocalSettings. It is NOT tied to any user identity; it
    /// exists only so a user who opted into crash reporting can ask us to
    /// delete their reports (PRIVACY.md → "Your choices"). Shown in
    /// Settings → Info → Build.
    /// </summary>
    private static string GetOrCreateInstallId()
    {
        string id = SettingsService.Get<string>("Settings_InstallId", string.Empty);
        if (string.IsNullOrEmpty(id))
        {
            id = Guid.NewGuid().ToString("N");
            SettingsService.Set("Settings_InstallId", id);
        }
        return id;
    }

    /// <summary>
    /// Apply the persisted language preference to
    /// <c>ApplicationLanguages.PrimaryLanguageOverride</c> so MRT loads
    /// the matching .resw folder. We persist a short tag ("ko" / "zh" /
    /// "pt") in Settings_Language; this method maps to the full IETF
    /// tag ("ko-KR" / "zh-CN" / "pt-BR") that the runtime expects.
    /// Mirrors GeneralSettingsSection.ShortTagToIetf — keep them in sync.
    ///
    /// Best-effort: PrimaryLanguageOverride is package-identity gated
    /// and throws InvalidOperationException / COMException in unpackaged
    /// dev runs. Falling back to the system locale is the safe degrade.
    /// </summary>
    private static void ApplyLanguageOverride()
    {
        try
        {
            string shortTag = SettingsService.Get<string>("Settings_Language", "");
            ApplicationLanguages.PrimaryLanguageOverride = shortTag switch
            {
                "ko" => "ko-KR",
                "en" => "en-US",
                "zh" => "zh-CN",
                "ja" => "ja-JP",
                "de" => "de-DE",
                "fr" => "fr-FR",
                "es" => "es-ES",
                "pt" => "pt-BR",
                "ru" => "ru-RU",
                "it" => "it-IT",
                _    => "",   // empty -> system default
            };
        }
        catch (InvalidOperationException) { /* unpackaged dev */ }
        catch (System.Runtime.InteropServices.COMException) { /* same */ }
    }

    /// <summary>
    /// Parse <c>--invoke &lt;verb&gt; --files "p1;p2;..."</c>. Returns
    /// <see langword="null"/> if the args don't carry a verb (normal launch).
    ///
    /// <para>
    /// Format-specific quick verbs (<c>compress-zip</c>, <c>compress-7z</c>)
    /// collapse to <c>Verb="compress"</c> + <c>QuickFormat=&lt;tag&gt;</c>
    /// + <c>IsHeadless=true</c> so MainWindow's verb dispatch only has
    /// to compare against the canonical roots.
    /// </para>
    /// </summary>
    internal static InvokeRequest? ParseInvokeArgs(string[] argv)
    {
        if (argv is null || argv.Length < 3)
        {
            return null;
        }
        string? verb = null;
        string? files = null;
        for (int i = 0; i < argv.Length - 1; i++)
        {
            if (string.Equals(argv[i], "--invoke", StringComparison.OrdinalIgnoreCase))
            {
                verb = argv[i + 1];
            }
            else if (string.Equals(argv[i], "--files", StringComparison.OrdinalIgnoreCase))
            {
                files = argv[i + 1];
            }
        }
        if (verb is null || files is null)
        {
            return null;
        }
        var paths = files
            .Split(';', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Where(p => p.Length > 0)
            .ToList();
        if (paths.Count == 0)
        {
            return null;
        }
        var (canonicalVerb, quickFormat, isHeadless) = NormalizeVerb(verb);
        return new InvokeRequest(canonicalVerb, paths, quickFormat, isHeadless);
    }

    /// <summary>
    /// Map a raw shell-extension verb string into the
    /// <see cref="InvokeRequest"/> shape: canonical verb root + optional
    /// quick-format tag + headless flag. Extracted from
    /// <see cref="ParseInvokeArgs"/> to keep that method under the
    /// MA0051 60-line cap (2026-05-19 4-context parity sprint added 4
    /// new verbs, which pushed it over).
    ///
    /// <para>
    /// Recognised verbs:
    /// <list type="bullet">
    /// <item><c>compress-zip</c> / <c>compress-7z</c> — collapse to
    /// <c>compress</c> + <c>QuickFormat</c>, headless (single-archive
    /// ProgressDialog path).</item>
    /// <item><c>compress-individually</c> — N-archive emit. NOT headless:
    /// the ProgressDialog builds a single CompressEngine plan, so to
    /// produce one archive per item we must route through
    /// MainWindow.DispatchInvokeAsync's per-path loop.</item>
    /// <item><c>extract-smart</c> / <c>extract-to-subfolder</c> — EXTRACT
    /// operations that pick their destination silently. NOT headless: the
    /// ProgressDialog only knows how to compress (CompressEngine), so an
    /// extract verb sent there would silently COMPRESS the archive instead
    /// of unpacking it (the 2026-06-17 bug). They route through
    /// MainWindow.DispatchInvokeAsync → ExtractAsync with the appropriate
    /// ShellExtractMode.</item>
    /// <item><c>compress</c> (plain, no QuickFormat) / <c>extract-dialog</c>
    /// — the "...(O)" / "...(B)" option verbs. Headless: v0.22 routes them
    /// to the dedicated <c>CompressOptionsDialog</c> /
    /// <c>ExtractOptionsDialog</c> (see <see cref="OnLaunched"/>), each of
    /// which owns its option form + an embedded JobProgressView. They no
    /// longer open the full MainWindow surface.</item>
    /// <item>Unrecognised → returned verbatim so MainWindow dispatch
    /// can produce a useful error.</item>
    /// </list>
    ///
    /// Headless == true now covers the two quick-compress verbs plus the
    /// two v0.22 option verbs; OnLaunched's verb switch picks the matching
    /// dialog. Everything else needs MainWindow's richer dispatch.
    /// </para>
    /// </summary>
    private static (string canonical, string? quickFormat, bool isHeadless) NormalizeVerb(string verb)
    {
        if (string.Equals(verb, "compress-zip", StringComparison.OrdinalIgnoreCase))
        {
            return ("compress", "zip", true);
        }
        if (string.Equals(verb, "compress-7z", StringComparison.OrdinalIgnoreCase))
        {
            return ("compress", "7z", true);
        }
        if (string.Equals(verb, "compress-individually", StringComparison.OrdinalIgnoreCase))
        {
            // NOT headless — needs the per-path N-archive loop in
            // DispatchInvokeAsync. Headless would make a single combined
            // archive, defeating "각 항목별 압축".
            return ("compress-individually", null, false);
        }
        if (string.Equals(verb, "extract-smart", StringComparison.OrdinalIgnoreCase))
        {
            // NOT headless — extract, not compress. See class remarks.
            return ("extract-smart", null, false);
        }
        if (string.Equals(verb, "extract-to-subfolder", StringComparison.OrdinalIgnoreCase))
        {
            return ("extract-to-subfolder", null, false);
        }
        if (string.Equals(verb, "extract-dialog", StringComparison.OrdinalIgnoreCase))
        {
            // v0.22: headless → ExtractOptionsDialog (no QuickFormat).
            return ("extract-dialog", null, true);
        }
        if (string.Equals(verb, "compress", StringComparison.OrdinalIgnoreCase))
        {
            // v0.22: plain compress (no quick-format tag) → headless
            // CompressOptionsDialog. The quick verbs above return earlier
            // with QuickFormat set, so this only catches the "...(O)" verb.
            return ("compress", null, true);
        }
        return (verb, null, false);
    }
}
