using System;
using System.Globalization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;

namespace OtterZip.App.UserControls;

/// <summary>
/// ConfigPanel — Concept C canvas + Compress-Options flyout.
///
/// Top strip hosts Format + Compress-Options ▾ + Settings ⚙ as chips.
/// The drop hint fills the rest. All compress-time tweaks (split,
/// password+AES, four meta checkboxes) live inside the
/// CompressOptionsButton's flyout — no expander on the main canvas.
/// </summary>
public sealed partial class ConfigPanel : UserControl
{
    /// <summary>Raised when the user clicks the empty drop-hint card.
    /// MainWindow handles the file-picker fallback so the panel stays
    /// decoupled from picker APIs.</summary>
    public event EventHandler? DropHintTapped;

    /// <summary>Raised when the user clicks the gear chip in the
    /// corner-strip. MainWindow opens the Settings surface.</summary>
    public event EventHandler? SettingsRequested;

    public ConfigPanel()
    {
        InitializeComponent();
        // Hydrate the panel from saved Settings defaults before first
        // paint so the user sees their preference rather than the XAML
        // hard-coded fallback. Loaded fires too late — the user could
        // glance and start a job before defaults apply.
        ApplyDefaultsFromSettings();
        ApplyFormatConstraints();
        ApplyCompressPasswordPrefill();
        // Live-update: when the user flips the relevant toggles in the
        // Settings window, sync the flyout's password field on the spot
        // so they don't have to restart to see the change.
        Services.SettingsService.Changed += OnSettingsChanged;
    }

    private void OnSettingsChanged(object? sender, Services.SettingsChangedEventArgs e)
    {
        if (string.Equals(e.Key, "Settings_DefaultPasswordOnCompress", StringComparison.Ordinal)
            || string.Equals(e.Key, "Settings_AuthBeforeUseDefaultPassword", StringComparison.Ordinal))
        {
            ApplyCompressPasswordPrefill();
        }
    }

    /// <summary>
    /// Mirror the Compress-Options password field with the stored
    /// default — but only when the user opted into "신규 압축에 자동
    /// 적용" (Settings_DefaultPasswordOnCompress). Skipped when the
    /// Hello-gate is on so a pre-filled value can't be revealed
    /// (PasswordBox has a reveal toggle in this flyout) without the
    /// auth the user asked for. Toggle-OFF clears the field so a stale
    /// pre-fill from a previous "ON" state doesn't leak as ConfigPanel.
    /// Password on the next compress.
    /// </summary>
    private void ApplyCompressPasswordPrefill()
    {
        bool autoApply = Services.SettingsService.Get<bool>("Settings_DefaultPasswordOnCompress", false);
        bool helloGate = Services.SettingsService.Get<bool>("Settings_AuthBeforeUseDefaultPassword", false);
        if (autoApply && !helloGate)
        {
            string stored = Services.CredentialStore.Get();
            PasswordInput.Password = stored;
        }
        else
        {
            PasswordInput.Password = string.Empty;
        }
    }

    private void OnDropHintTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        DropHintTapped?.Invoke(this, EventArgs.Empty);
    }

    private void OnSettingsChipClick(object sender, RoutedEventArgs e)
    {
        SettingsRequested?.Invoke(this, EventArgs.Empty);
    }

    // --- "More" menu (⋯): support + sister apps + help. Each item opens an
    //     external URL (GitHub Sponsors / Microsoft Store / README) in the
    //     default handler. Best-effort — a missing browser is non-fatal. ---
    private static async void OpenUri(string uri)
    {
        try { await Windows.System.Launcher.LaunchUriAsync(new Uri(uri)); }
        catch { /* no browser / protocol handler — nothing we can do, don't crash */ }
    }

    private void OnMoreSupport(object sender, RoutedEventArgs e)
        => OpenUri("https://github.com/sponsors/LumiBearStudio");

    private void OnMoreSpanFinder(object sender, RoutedEventArgs e)
        => OpenUri("https://apps.microsoft.com/detail/9P7NJ351X9TL");

    private void OnMoreOtterGif(object sender, RoutedEventArgs e)
        => OpenUri("https://apps.microsoft.com/detail/9N0DSSBVCN81");

    private void OnMoreHelp(object sender, RoutedEventArgs e)
        => OpenUri("https://github.com/LumiBearStudio/OtterZip#readme");

    /// <summary>
    /// Single handler for every checkbox inside the Compress-Options
    /// flyout. The checkbox's Tag is its
    /// <see cref="Services.SettingsService"/> key — keeps the panel and
    /// Settings dialog in lock-step, since both write to the same store.
    /// </summary>
    private void OnAdvancedToggle(object sender, RoutedEventArgs e)
    {
        if (sender is CheckBox cb && cb.Tag is string key)
        {
            Services.SettingsService.Set(key, cb.IsChecked.GetValueOrDefault());
        }
    }

    private void ApplyDefaultsFromSettings()
    {
        // Format default — match the ComboBoxItem index by Tag.
        string fmt = Services.SettingsService.Get<string>("Settings_DefaultFormat", "ZIP");
        for (int i = 0; i < FormatCombo.Items.Count; i++)
        {
            if (FormatCombo.Items[i] is ComboBoxItem item
                && item.Tag is string tag
                && string.Equals(tag, fmt, StringComparison.Ordinal))
            {
                FormatCombo.SelectedIndex = i;
                break;
            }
        }

        // Compress-Options flyout checkboxes — keep in lock-step with
        // Settings → Compression so both surfaces show the same state.
        ExcludeMetaCheck.IsChecked   = Services.SettingsService.Get<bool>("Settings_ExcludeSystemMetadata", true);
        VerifyCheck.IsChecked        = Services.SettingsService.Get<bool>("Settings_VerifyAfterCompress", false);
        DeleteSourceCheck.IsChecked  = Services.SettingsService.Get<bool>("Settings_DeleteSourceAfterCompress", false);
        SeparatelyCheck.IsChecked    = Services.SettingsService.Get<bool>("Settings_CompressSeparately", false);
    }

    // ============================================================
    //  Format
    // ============================================================
    public static readonly DependencyProperty SelectedFormatProperty =
        DependencyProperty.Register(
            nameof(SelectedFormat),
            typeof(string),
            typeof(ConfigPanel),
            new PropertyMetadata("ZIP"));

    public string SelectedFormat
    {
        get => (string)GetValue(SelectedFormatProperty);
        set => SetValue(SelectedFormatProperty, value);
    }

    private void OnFormatChanged(object sender, SelectionChangedEventArgs e)
    {
        if (FormatCombo.SelectedItem is ComboBoxItem item && item.Tag is string tag)
        {
            SelectedFormat = tag;
        }
        ApplyFormatConstraints();
    }

    // ============================================================
    //  Split size
    // ============================================================
    public static readonly DependencyProperty SplitSizeBytesProperty =
        DependencyProperty.Register(
            nameof(SplitSizeBytes),
            typeof(long),
            typeof(ConfigPanel),
            new PropertyMetadata(0L));

    public long SplitSizeBytes
    {
        get => (long)GetValue(SplitSizeBytesProperty);
        set => SetValue(SplitSizeBytesProperty, value);
    }

    private void OnSplitChanged(object sender, SelectionChangedEventArgs e)
    {
        if (SplitCombo.SelectedItem is ComboBoxItem item
            && item.Tag is string tag
            && long.TryParse(tag, NumberStyles.Integer, CultureInfo.InvariantCulture, out long value))
        {
            SplitSizeBytes = value;
        }
    }

    // ============================================================
    //  Password + AES
    // ============================================================
    public static readonly DependencyProperty PasswordProperty =
        DependencyProperty.Register(
            nameof(Password),
            typeof(string),
            typeof(ConfigPanel),
            new PropertyMetadata(string.Empty));

    public string Password
    {
        get => (string)GetValue(PasswordProperty);
        set => SetValue(PasswordProperty, value);
    }

    private void OnPasswordChanged(object sender, RoutedEventArgs e)
    {
        Password = PasswordInput.Password ?? string.Empty;
        // Encryption is always AES-256 (fail-closed core) — the note tells
        // the user the output won't open in stock Windows Explorer. The old
        // UseAes256 checkbox/DP were wired to nothing and got removed.
        if (AesNoteText is not null)
        {
            AesNoteText.Visibility =
                !string.IsNullOrEmpty(Password) && FormatSupportsPassword(SelectedFormat)
                    ? Visibility.Visible
                    : Visibility.Collapsed;
        }
    }

    private void OnRevealToggled(object sender, RoutedEventArgs e)
    {
        PasswordInput.PasswordRevealMode = RevealButton.IsChecked.GetValueOrDefault()
            ? PasswordRevealMode.Visible
            : PasswordRevealMode.Hidden;
    }

    // ============================================================
    //  Bottom checkboxes (DependencyProperties)
    // ============================================================
    public static readonly DependencyProperty ExcludeSystemMetaProperty =
        DependencyProperty.Register(
            nameof(ExcludeSystemMeta),
            typeof(bool),
            typeof(ConfigPanel),
            new PropertyMetadata(true));

    public bool ExcludeSystemMeta
    {
        get => (bool)GetValue(ExcludeSystemMetaProperty);
        set => SetValue(ExcludeSystemMetaProperty, value);
    }

    public static readonly DependencyProperty VerifyAfterCompressProperty =
        DependencyProperty.Register(
            nameof(VerifyAfterCompress),
            typeof(bool),
            typeof(ConfigPanel),
            new PropertyMetadata(false));

    public bool VerifyAfterCompress
    {
        get => (bool)GetValue(VerifyAfterCompressProperty);
        set => SetValue(VerifyAfterCompressProperty, value);
    }

    public static readonly DependencyProperty DeleteSourceAfterProperty =
        DependencyProperty.Register(
            nameof(DeleteSourceAfter),
            typeof(bool),
            typeof(ConfigPanel),
            new PropertyMetadata(false));

    public bool DeleteSourceAfter
    {
        get => (bool)GetValue(DeleteSourceAfterProperty);
        set => SetValue(DeleteSourceAfterProperty, value);
    }

    public static readonly DependencyProperty CompressSeparatelyProperty =
        DependencyProperty.Register(
            nameof(CompressSeparately),
            typeof(bool),
            typeof(ConfigPanel),
            new PropertyMetadata(false));

    public bool CompressSeparately
    {
        get => (bool)GetValue(CompressSeparatelyProperty);
        set => SetValue(CompressSeparatelyProperty, value);
    }

    // ============================================================
    //  Format-driven enable/disable (tar / gz / xz can't password-protect or split)
    // ============================================================
    private void ApplyFormatConstraints()
    {
        // FormatCombo's SelectionChanged fires while XAML is still loading
        // top-to-bottom — at that moment PasswordCard / SplitCombo may not
        // exist yet. Skip until the visual tree is fully realized.
        if (PasswordInput is null || RevealButton is null || AesNoteText is null || SplitCombo is null)
        {
            return;
        }

        bool supportsPassword = FormatSupportsPassword(SelectedFormat);
        bool supportsSplit = FormatSupportsSplit(SelectedFormat);

        PasswordInput.IsEnabled = supportsPassword;
        RevealButton.IsEnabled = supportsPassword;
        if (!supportsPassword)
        {
            // Clear, don't just disable: a stale password + tar/xz would hit
            // the core's fail-closed guard and error the whole job (the
            // options dialog already cleared; the flyout didn't).
            PasswordInput.Password = string.Empty;
        }
        AesNoteText.Visibility =
            supportsPassword && !string.IsNullOrEmpty(PasswordInput.Password)
                ? Visibility.Visible
                : Visibility.Collapsed;

        SplitCombo.IsEnabled = supportsSplit;
        if (!supportsSplit && SplitCombo.SelectedIndex != 0)
        {
            SplitCombo.SelectedIndex = 0;
        }
    }

    private static bool FormatSupportsPassword(string format) =>
        format is "ZIP" or "7z";

    private static bool FormatSupportsSplit(string format) =>
        format is "ZIP" or "7z";

    // ============================================================
    //  Card hover micro-animation
    // ============================================================
    private void OnCardPointerEntered(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (sender is Border b)
        {
            b.BorderBrush = (Brush)Application.Current.Resources["OtterzipBrandBrush"];
        }
    }

    private void OnCardPointerExited(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (sender is Border b)
        {
            b.BorderBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        }
    }
}
