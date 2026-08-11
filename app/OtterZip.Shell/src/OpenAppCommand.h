// OtterZip.Shell — IExplorerCommand for the empty-space "OtterZip 열기"
// verb (Directory\Background only).
//
// 2026-06-18: replaces the short-lived "새 폴더" background verb. New
// Folder duplicated Windows' native top-of-menu entry and packaged verbs
// can't be pinned to the top (desktop4:Verb has no position attribute), so
// instead the single background entry launches the OtterZip main window —
// a non-redundant action whose mid-menu placement is fine.

#pragma once

#include <windows.h>
#include <shobjidl_core.h>
#include <winrt/base.h>

namespace OtterZip::Shell
{
    struct __declspec(uuid("ad0f669b-3701-40c6-87b2-0aacc445dff7"))
    OpenAppCommand : winrt::implements<OpenAppCommand, IExplorerCommand>
    {
        // IExplorerCommand
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
