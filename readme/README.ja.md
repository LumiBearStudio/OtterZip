<div align="center">

<img src="images/otter.png" width="96" alt="OtterZip">

# OtterZip

**Windows のための、静かなアーカイブツール。**

右クリックで圧縮、右クリックで展開。広告なし、アカウントなし、トラッキングなし。

[**ウェブサイト**](https://lumibear.dev/apps/otterzip/) · [**Microsoft Store**](https://apps.microsoft.com/detail/9NWQNGGSWJCL) · [**ダウンロード**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)

[English](../README.md) · [한국어](README.ko.md) · 日本語 · [中文](README.zh.md) · [Deutsch](README.de.md) · [Français](README.fr.md) · [Español](README.es.md) · [Português](README.pt.md) · [Русский](README.ru.md) · [Italiano](README.it.md)

</div>

<br>

<img src="images/window.png" alt="OtterZip main window">

## 圧縮。完了。

ファイルを右クリック — 圧縮完了。アーカイブを右クリック — 展開完了。アプリを開くことは、ほとんどありません。

## なぜ OtterZip か

- **エクスプローラーから直接** — 右クリックメニューからそのまま圧縮・展開。先にウィンドウを開く必要はありません。
- **ひとつのことを、きちんと** — モードなし、余計な要素なし、覚えることもなし。
- **広告なし。アカウントなし。トラッキングなし。** 抱き合わせインストールも、しつこい通知も、登録も一切ありません。クラッシュレポートはオプトインのみです。
- **ネイティブコアは、静かに速い** — エンジンは Rust で書かれています。仕事を終えて、待たせません。
- **Windows らしく見えるのは、そのものだから** — C# と WinUI 3 で作られています。ウィンドウの中の Web ページではなく、本物のネイティブインターフェイスです。
- **好みに合わせて** — ライト、ダーク、システムに追従。10 言語を内蔵。

## インストール

### Microsoft Store — 推奨

ワンクリックでインストール、更新は自動で届きます。

[Microsoft Store から入手](https://apps.microsoft.com/detail/9NWQNGGSWJCL)

### 直接ダウンロード — 無料

1. [**OtterZip_x64_installer.zip**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip) をダウンロードします
2. zip を展開します。
3. `Install.ps1` を右クリック → **PowerShell で実行**、表示される確認に同意します。

このバンドルは Store ではなく LumiBear Studio が署名しているため、初回インストール時に発行元の証明書を一度だけ登録します。確認が表示されるのは、そのためです。手順の詳細: [**インストール方法**](https://lumibear.dev/apps/otterzip/#install)

どちらの入手経路でも、アプリはまったく同じです。

## 対応形式

**作成** — ZIP · 7z · TAR · TAR.GZ

**展開** — ZIP · 7z · RAR · TAR · TAR.GZ · GZ · BZ2 · XZ · ZST · LZ4 · ISO · CAB · JAR · APK · IPA ほか

必要なときには、AES-256 暗号化と分割（マルチボリューム）アーカイブも。

## 最初から、ちょうどいい設定で

<img src="images/settings.png" width="620" alt="OtterZip settings">

テーマ、言語、上書きのルールなど — 好きなように調整でき、必要になるまでは表に出ません。

## 動作環境

Windows 10（バージョン 2004）以降 · x64

## ライセンス

- `crates/**` — **MIT OR Apache-2.0**（Rust デュアルライセンス）
- `app/**` — **GPL-3.0-or-later** (+ unRAR exception)

詳細は [LICENSING.md](../LICENSING.md) をご覧ください。

## サードパーティ通知

[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) をご覧ください。特に **unrar**（© Alexander Roshal）は RAR の展開にのみ使用しています — OtterZip が RAR アーカイブを作成することはありません。

## プライバシー

広告なし、アカウントなし、トラッキングなし。クラッシュレポートはオプトインで、既定ではオフです。[PRIVACY.md](../PRIVACY.md) をご覧ください。

---

<div align="center">

© 2026 LumiBear Studio

</div>
