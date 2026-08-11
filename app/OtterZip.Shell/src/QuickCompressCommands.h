// OtterZip.Shell — Bandizip-style quick-compress verbs.
//
// Two IExplorerCommand classes that produce the format-specific direct
// "DATA.zip으로 압축(&Z)" / "DATA.7z으로 압축(&7)" entries shown at the
// top of Explorer's right-click menu. The third "OtterZip으로
// 압축..." verb stays as `CompressCommand` since it still opens the
// host MainWindow (dialog flow).
//
// Each class registers under its own CLSID so the Package.appxmanifest
// can wire `<desktop4:Verb>` per shape. Both invocations route to the
// host EXE with a format-specific `--invoke compress-zip` /
// `--invoke compress-7z` arg, parsed by App.ParseInvokeArgs (host
// side) into `InvokeRequest { Verb="compress", QuickFormat=..., IsHeadless=true }`.

#pragma once

#include <windows.h>
#include <shobjidl_core.h>
#include <winrt/base.h>

namespace OtterZip::Shell
{
    struct __declspec(uuid("d38f2276-845d-499d-8311-3c9c9ba692d3"))
    CompressZipQuickCommand : winrt::implements<CompressZipQuickCommand, IExplorerCommand>
    {
        IFACEMETHODIMP GetTitle(IShellItemArray* items, LPWSTR* ppszName) noexcept override;
        IFACEMETHODIMP GetIcon(IShellItemArray* items, LPWSTR* ppszIcon) noexcept override;
        IFACEMETHODIMP GetToolTip(IShellItemArray* items, LPWSTR* ppszInfotip) noexcept override;
        IFACEMETHODIMP GetCanonicalName(GUID* pguidCommandName) noexcept override;
        IFACEMETHODIMP GetState(IShellItemArray* items, BOOL fOkToBeSlow, EXPCMDSTATE* pCmdState) noexcept override;
        IFACEMETHODIMP Invoke(IShellItemArray* items, IBindCtx* pbc) noexcept override;
        IFACEMETHODIMP GetFlags(EXPCMDFLAGS* pFlags) noexcept override;
        IFACEMETHODIMP EnumSubCommands(IEnumExplorerCommand** ppEnum) noexcept override;
    };

    struct __declspec(uuid("48492b43-300e-41a2-9392-356f3debc2f5"))
    CompressSevenZQuickCommand : winrt::implements<CompressSevenZQuickCommand, IExplorerCommand>
    {
        IFACEMETHODIMP GetTitle(IShellItemArray* items, LPWSTR* ppszName) noexcept override;
        IFACEMETHODIMP GetIcon(IShellItemArray* items, LPWSTR* ppszIcon) noexcept override;
        IFACEMETHODIMP GetToolTip(IShellItemArray* items, LPWSTR* ppszInfotip) noexcept override;
        IFACEMETHODIMP GetCanonicalName(GUID* pguidCommandName) noexcept override;
        IFACEMETHODIMP GetState(IShellItemArray* items, BOOL fOkToBeSlow, EXPCMDSTATE* pCmdState) noexcept override;
        IFACEMETHODIMP Invoke(IShellItemArray* items, IBindCtx* pbc) noexcept override;
        IFACEMETHODIMP GetFlags(EXPCMDFLAGS* pFlags) noexcept override;
        IFACEMETHODIMP EnumSubCommands(IEnumExplorerCommand** ppEnum) noexcept override;
    };

    /// Compute the basename shown as the selection stem in the
    /// right-click verb. Single source = its own name; multi = the
    /// common parent directory name (Bandizip parity, 2026-05-19).
    /// Falls back to first item's stem if parents are mixed or the
    /// parent is a root drive.
    std::wstring DeriveSelectionStem(IShellItemArray* items) noexcept;

    // --------------------- CompressIndividually --------------------
    // Bandizip's `각각 파일명/폴더명으로 압축하기(U)` — emits N archives
    // (one per selected item) using the user's default-format setting.
    // GetState hides itself on single selections; the cheaper
    // single-item case is already covered by the per-format quick verbs.
    struct __declspec(uuid("fc32ea28-1809-481c-b71e-d84b61229da0"))
    CompressIndividuallyCommand : winrt::implements<CompressIndividuallyCommand, IExplorerCommand>
    {
        IFACEMETHODIMP GetTitle(IShellItemArray* items, LPWSTR* ppszName) noexcept override;
        IFACEMETHODIMP GetIcon(IShellItemArray* items, LPWSTR* ppszIcon) noexcept override;
        IFACEMETHODIMP GetToolTip(IShellItemArray* items, LPWSTR* ppszInfotip) noexcept override;
        IFACEMETHODIMP GetCanonicalName(GUID* pguidCommandName) noexcept override;
        IFACEMETHODIMP GetState(IShellItemArray* items, BOOL fOkToBeSlow, EXPCMDSTATE* pCmdState) noexcept override;
        IFACEMETHODIMP Invoke(IShellItemArray* items, IBindCtx* pbc) noexcept override;
        IFACEMETHODIMP GetFlags(EXPCMDFLAGS* pFlags) noexcept override;
        IFACEMETHODIMP EnumSubCommands(IEnumExplorerCommand** ppEnum) noexcept override;
    };
}
