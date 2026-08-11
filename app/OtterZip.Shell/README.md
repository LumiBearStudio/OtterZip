# OtterZip.Shell — Windows Explorer Shell Extension (Sprint 4 scaffold)

C++/WinRT COM in-proc DLL exposing three `IExplorerCommand` verbs:

| Verb | CLSID | Action |
|------|-------|--------|
| `OtterzipExtractHere` | `{e60e719c-1cbb-4651-a374-eff2d5ddde9b}` | Extract archive to a sibling folder |
| `OtterzipExtractTo` | `{81df37ed-1fd3-4e3a-8cf2-d5f8bfb644b8}` | Extract archive to a folder picker target |
| `OtterzipCompress` | `{a5927606-6461-438c-81a2-e1205640d703}` | Compress selected files/folders into a ZIP |

Per `docs/03-api/shell-extension.md`, these are registered through MSIX
(`uap3:FileExplorerContextMenus`). For local development without MSIX, the
DLL can be hand-registered with `regsvr32` once `IExplorerCommand` plumbing
lands in Sprint 5.

## Sprint 4 status

- ✅ `vcxproj` scaffold with Windows App SDK + C++/WinRT references
- ✅ `dllmain.cpp` + skeleton `IExplorerCommand` implementation files
- ✅ Package manifest fragment for MSIX integration
- ⏳ Verb invoke logic — calls `OtterZip.exe --invoke <verb>` (Sprint 5)
- ⏳ MSIX packaging integration (Sprint 5)
- ⏳ COM registration test (Sprint 5)

## Build prerequisites

- Visual Studio 2022 17.8+ with C++/WinRT workload
- Windows SDK 10.0.22621
- Microsoft.Windows.CppWinRT 2.0.x NuGet package (auto-restored)

## Local invocation contract

Each verb spawns the host app with a CLI:

```
OtterZip.exe --invoke <verb> --files <path>[;<path>...]
```

Verbs are documented in `docs/03-api/shell-extension.md` §4.
