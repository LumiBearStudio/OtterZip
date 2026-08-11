// OtterZip.Shell — top-level "OtterZip" parent verb that hosts a submenu
// containing Compress + Extract here children. Active when
// Settings_ShellMenuMode == "nested".
//
// Layout in Explorer's right-click menu:
//
//   Open
//   Open with...
//   ...
//   OtterZip               <-- this parent (ECF_HASSUBCOMMANDS)
//      └─ Compress with OtterZip
//      └─ Extract here (OtterZip)    (only on archive items)
//
// Flat-mode behavior (default user-visible toggle = "nested" matches the
// C# Settings default, see ShellSettings::IsShellMenuNested):
//   - Parent reports ECS_HIDDEN; children report ECS_ENABLED individually
//
// Nested-mode behavior:
//   - Parent reports ECS_ENABLED; children report ECS_HIDDEN at the top
//     level so they only show via the parent's EnumSubCommands path

#pragma once

#include <windows.h>
#include <shobjidl_core.h>
#include <winrt/base.h>

namespace OtterZip::Shell
{
    struct __declspec(uuid("81df37ed-1fd3-4e3a-8cf2-d5f8bfb644b8"))
    OtterzipMenuCommand : winrt::implements<OtterzipMenuCommand, IExplorerCommand>
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
