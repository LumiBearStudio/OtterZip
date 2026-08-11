// OtterZip.Shell — parent IExplorerCommand for the nested-mode submenu.
//
// Visibility decision (GetState):
//   - Master toggle off (Settings_ShellMenuEnabled)         -> ECS_HIDDEN
//   - Mode is "flat" (Settings_ShellMenuMode != "nested")   -> ECS_HIDDEN
//   - Otherwise (nested mode, master on)                    -> ECS_ENABLED
//
// Invoke is a no-op: the user clicks the parent only to expand the
// submenu — actual work happens in the children's Invoke handlers.
//
// CLSID 81df37ed-1fd3-4e3a-8cf2-d5f8bfb644b8 is registered in the MSIX
// manifest as a third com:Class + windows.fileExplorerContextMenus Verb.

#include "OtterzipMenuCommand.h"
#include "SubmenuCommands.h"
#include "ShellAssets.h"
#include "ShellSettings.h"
#include "ShellStrings.h"

#include <shlwapi.h>     // SHStrDupW

#pragma comment(lib, "Shlwapi.lib")

namespace OtterZip::Shell
{
    IFACEMETHODIMP OtterzipMenuCommand::GetTitle(IShellItemArray*, LPWSTR* ppszName) noexcept
    {
        // The parent menu label is the brand name; per CLAUDE.md the brand
        // name itself is not translated. Still routed through ResourceLoader
        // so a future build that wants to append "(OtterZip)" or "OtterZip ▶"
        // in non-Latin locales has a single hook point.
        std::wstring title = LoadShellString(L"Shell_OtterzipMenu_Title", L"OtterZip");
        return SHStrDupW(title.c_str(), ppszName);
    }

    IFACEMETHODIMP OtterzipMenuCommand::GetIcon(IShellItemArray*, LPWSTR* ppszIcon) noexcept
    {
        // Parent submenu icon — appears next to the "OtterZip ▶" entry
        // in nested mode. Same brand icon as the leaf verbs so the
        // submenu visually anchors to the cluster.
        if (!ppszIcon) return E_POINTER;
        std::wstring path = GetPackagedAssetPath(L"Assets\\AppIcon.ico");
        if (path.empty())
        {
            *ppszIcon = nullptr;
            return E_NOTIMPL;
        }
        return SHStrDupW(path.c_str(), ppszIcon);
    }

    IFACEMETHODIMP OtterzipMenuCommand::GetToolTip(IShellItemArray*, LPWSTR* ppszInfotip) noexcept
    {
        std::wstring tooltip = LoadShellString(
            L"Shell_OtterzipMenu_Tooltip",
            L"OtterZip — fast archive tool");
        return SHStrDupW(tooltip.c_str(), ppszInfotip);
    }

    IFACEMETHODIMP OtterzipMenuCommand::GetCanonicalName(GUID* pguidCommandName) noexcept
    {
        if (!pguidCommandName) return E_POINTER;
        *pguidCommandName = __uuidof(OtterzipMenuCommand);
        return S_OK;
    }

    IFACEMETHODIMP OtterzipMenuCommand::GetState(IShellItemArray* items, BOOL, EXPCMDSTATE* pCmdState) noexcept
    {
        if (!pCmdState) return E_POINTER;
        // Master gate first — same precedence as the leaf commands.
        if (!IsShellMenuEnabled())
        {
            *pCmdState = ECS_HIDDEN;
            return S_OK;
        }
        // Only show in nested mode; flat mode keeps the leaf verbs at top
        // level instead.
        if (!IsShellMenuNested())
        {
            *pCmdState = ECS_HIDDEN;
            return S_OK;
        }
        DWORD count = 0;
        if (items && SUCCEEDED(items->GetCount(&count)) && count > 0)
        {
            *pCmdState = ECS_ENABLED;
        }
        else
        {
            *pCmdState = ECS_HIDDEN;
        }
        return S_OK;
    }

    IFACEMETHODIMP OtterzipMenuCommand::Invoke(IShellItemArray*, IBindCtx*) noexcept
    {
        // No-op. Parent menu items aren't directly invokable when they
        // carry sub-commands; Explorer expands the submenu instead.
        return S_OK;
    }

    IFACEMETHODIMP OtterzipMenuCommand::GetFlags(EXPCMDFLAGS* pFlags) noexcept
    {
        if (!pFlags) return E_POINTER;
        // ECF_HASSUBCOMMANDS tells Explorer "I'm a fly-out parent — call
        // EnumSubCommands to populate me, don't invoke me directly".
        *pFlags = ECF_HASSUBCOMMANDS;
        return S_OK;
    }

    IFACEMETHODIMP OtterzipMenuCommand::EnumSubCommands(IEnumExplorerCommand** ppEnum) noexcept
    {
        if (!ppEnum) return E_POINTER;
        *ppEnum = nullptr;
        try
        {
            auto enumerator = winrt::make<SubmenuEnumerator>();
            return enumerator->QueryInterface(IID_PPV_ARGS(ppEnum));
        }
        catch (winrt::hresult_error const& e) { return e.code(); }
        catch (...) { return E_FAIL; }
    }
}
