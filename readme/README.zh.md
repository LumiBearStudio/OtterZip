<div align="center">

<img src="images/otter.png" width="96" alt="OtterZip">

# OtterZip

**安静的 Windows 压缩工具。**

右键即可压缩或解压。无广告，无账户，无跟踪。

[**网站**](https://lumibear.dev/apps/otterzip/) · [**Microsoft Store**](https://apps.microsoft.com/detail/9NWQNGGSWJCL) · [**下载**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)

[English](../README.md) · [한국어](README.ko.md) · [日本語](README.ja.md) · 中文 · [Deutsch](README.de.md) · [Français](README.fr.md) · [Español](README.es.md) · [Português](README.pt.md) · [Русский](README.ru.md) · [Italiano](README.it.md)

</div>

<br>

<img src="images/window.png" alt="OtterZip main window">

## 一压，搞定。

右键点一个文件，它就压好了。右键点一个压缩包，内容就出来了。你几乎不用打开这个应用。

## 为什么选择 OtterZip

- **直接在资源管理器里** — 在右键菜单里直接压缩和解压，不必先打开窗口。
- **只做一件事，并做好** — 没有模式切换，没有杂乱界面，没有学习成本。
- **无广告。无账户。无跟踪。** 不捆绑软件，不反复弹窗，不需要注册。崩溃报告严格采用选择加入。
- **原生内核，安静而迅捷** — 引擎用 Rust 编写。它把活干完，不让你等待。
- **看起来像 Windows，因为它就是** — 由 C# 与 WinUI 3 构建。真正的原生界面，而不是套在窗口里的网页。
- **随你调整** — 浅色、深色，或跟随系统。内置十种语言。

## 安装

### Microsoft Store — 推荐

一键安装，更新自动送达。

[从 Microsoft Store 获取](https://apps.microsoft.com/detail/9NWQNGGSWJCL)

### 直接下载 — 免费

1. 下载 [**OtterZip_x64_installer.zip**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)
2. 解压该 zip 文件。
3. 右键点击 `Install.ps1` → **使用 PowerShell 运行**，然后同意提示。

该安装包由 LumiBear Studio 签名，而非由 Store 签名，因此首次安装会注册一次我们的发布者证书 — 提示就是为此而来。分步指南：[**如何安装**](https://lumibear.dev/apps/otterzip/#install)

两个渠道是完全相同的应用。

## 格式

**创建** — ZIP · 7z · TAR · TAR.GZ

**解压** — ZIP · 7z · RAR · TAR · TAR.GZ · GZ · BZ2 · XZ · ZST · LZ4 · ISO · CAB · JAR · APK · IPA 等

需要时，还有 AES-256 加密与分卷压缩包。

## 开箱即用的合理默认

<img src="images/settings.png" width="620" alt="OtterZip settings">

主题、语言、覆盖规则等等 — 随你调整，不需要时就不会打扰你。

## 系统要求

Windows 10（版本 2004）或更高 · x64

## 许可

- `crates/**` — **MIT OR Apache-2.0**（Rust 双许可）
- `app/**` — **GPL-3.0-or-later** (+ unRAR exception)

详见 [LICENSING.md](../LICENSING.md)。

## 第三方声明

参见 [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md)。其中 **unrar**（© Alexander Roshal）仅用于 RAR 解压 — OtterZip 从不创建 RAR 压缩包。

## 隐私

无广告，无账户，无跟踪。崩溃报告为选择加入，默认关闭。参见 [PRIVACY.md](../PRIVACY.md)。

---

<div align="center">

© 2026 LumiBear Studio

</div>
