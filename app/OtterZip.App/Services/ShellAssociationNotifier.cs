using System;
using System.Runtime.InteropServices;
using Windows.ApplicationModel;

namespace OtterZip.App.Services;

/// <summary>
/// Nudges Explorer to reload shell associations the first time the app runs
/// after an install or update.
///
/// <para>Why: our context-menu verbs are packaged <c>com:Class</c> handlers.
/// Right after an MSIX install/update — especially one that CHANGES the verb
/// CLSIDs — Explorer keeps the previous registration cached, so the verbs are
/// missing until the shell is re-initialized (a sign-out or reboot). MSIX has
/// no post-install code hook, so the earliest we can run is the first app
/// launch; there we broadcast <c>SHCNE_ASSOCCHANGED</c> once to ask Explorer
/// to flush its association caches without the user rebooting.</para>
///
/// <para>Honest scope: this refreshes the classic association cache and may
/// not fully reload the packaged-COM activation catalog (managed by the AppX
/// State Repository), so on some systems a reboot can still be required. It is
/// cheap and non-disruptive, so we try it regardless. It does NOT address the
/// separate once-per-boot first-right-click miss, which is an Explorer-side
/// platform limit (see <see cref="ShellMenuCache"/> remarks). Gated by the
/// last-notified version so it fires once per install/update, not every
/// launch.</para>
/// </summary>
internal static class ShellAssociationNotifier
{
    private const string LastVersionKey = "Settings_LastShellNotifyVersion";

    // shell32!SHChangeNotify — broadcasts a shell change event. Matches the
    // OtterZip.App project's [DllImport] Win32 convention (the MSIX ships
    // non-AOT; see the csproj IL2026/IL3050 note). SHChangeNotify has no A/W
    // variants, so ExactSpelling resolves the export by its literal name.
    private const int SHCNE_ASSOCCHANGED = 0x08000000;
    private const uint SHCNF_IDLIST = 0x0000;

    [DllImport("shell32.dll", ExactSpelling = true)]
    private static extern void SHChangeNotify(int wEventId, uint uFlags, IntPtr dwItem1, IntPtr dwItem2);

    /// <summary>
    /// If the running package version differs from the last one we notified
    /// for, broadcast <c>SHCNE_ASSOCCHANGED</c> once and record the new
    /// version. Best-effort; never throws (<see cref="Package.Current"/>
    /// throws in unpackaged dev runs, where there is no shell to notify).
    /// </summary>
    public static void NotifyIfVersionChanged()
    {
        try
        {
            PackageVersion v = Package.Current.Id.Version;
            string current = $"{v.Major}.{v.Minor}.{v.Build}.{v.Revision}";
            string last = SettingsService.Get<string>(LastVersionKey, string.Empty);
            if (string.Equals(current, last, StringComparison.Ordinal))
            {
                return;
            }

            SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, IntPtr.Zero, IntPtr.Zero);

            // Record only after the broadcast so a failure before this point
            // simply retries on the next launch (the notify is idempotent).
            SettingsService.Set(LastVersionKey, current);
        }
        catch (Exception)
        {
            // Unpackaged dev run (no package identity / no shell) or any
            // shell hiccup — this is a non-essential nudge; the reboot
            // fallback still applies.
        }
    }
}
