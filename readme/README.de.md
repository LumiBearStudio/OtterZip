<div align="center">

<img src="images/otter.png" width="96" alt="OtterZip">

# OtterZip

**Das stille Archivwerkzeug für Windows.**

Mit einem Rechtsklick komprimieren oder entpacken. Keine Werbung, keine Konten, kein Tracking.

[**Website**](https://lumibear.dev/apps/otterzip/) · [**Microsoft Store**](https://apps.microsoft.com/detail/9NWQNGGSWJCL) · [**Download**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)

[English](../README.md) · [한국어](README.ko.md) · [日本語](README.ja.md) · [中文](README.zh.md) · Deutsch · [Français](README.fr.md) · [Español](README.es.md) · [Português](README.pt.md) · [Русский](README.ru.md) · [Italiano](README.it.md)

</div>

<br>

<img src="images/window.png" alt="OtterZip main window">

## Zippen. Fertig.

Rechtsklick auf eine Datei — sie ist gezippt. Rechtsklick auf ein Archiv — es ist entpackt. Die App selbst öffnen Sie kaum.

## Warum OtterZip

- **Direkt aus dem Explorer** — komprimieren und entpacken gleich aus dem Rechtsklickmenü. Kein Fenster, das erst aufgehen muss.
- **Eine Sache, richtig gemacht** — keine Modi, kein Ballast, keine Einarbeitung.
- **Keine Werbung. Keine Konten. Kein Tracking.** Keine Beipacksoftware, kein Nachhaken, nichts zum Anmelden. Absturzberichte sind ausschließlich opt-in.
- **Ein nativer Kern, still und schnell** — die Engine ist in Rust geschrieben. Sie erledigt die Arbeit und lässt Sie nicht warten.
- **Sieht aus wie Windows, weil es Windows ist** — gebaut mit C# und WinUI 3. Eine echte native Oberfläche, keine Webseite in einem Fenster.
- **Nach Ihrem Maß** — hell, dunkel oder dem System folgend. Zehn Sprachen sind eingebaut.

## Installation

### Microsoft Store — empfohlen

Installation mit einem Klick, Updates kommen automatisch.

[Im Microsoft Store holen](https://apps.microsoft.com/detail/9NWQNGGSWJCL)

### Direkter Download — kostenlos

1. Laden Sie [**OtterZip_x64_installer.zip**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip) herunter
2. Entpacken Sie die ZIP-Datei.
3. Rechtsklick auf `Install.ps1` → **Mit PowerShell ausführen**, dann die Abfrage bestätigen.

Das Paket ist von LumiBear Studio signiert und nicht vom Store, deshalb registriert die erste Installation einmalig unser Herausgeberzertifikat — dafür ist die Abfrage da. Schritt-für-Schritt-Anleitung: [**Anleitung zur Installation**](https://lumibear.dev/apps/otterzip/#install)

Beide Kanäle liefern exakt dieselbe App.

## Formate

**Erstellen** — ZIP · 7z · TAR · TAR.GZ

**Entpacken** — ZIP · 7z · RAR · TAR · TAR.GZ · GZ · BZ2 · XZ · ZST · LZ4 · ISO · CAB · JAR · APK · IPA und weitere

AES-256-Verschlüsselung und geteilte Archive (mehrere Volumes), wenn Sie sie brauchen.

## Sinnvolle Voreinstellungen, von Anfang an

<img src="images/settings.png" width="620" alt="OtterZip settings">

Design, Sprache, Überschreibregeln und mehr — eingestellt, wie Sie möchten, und aus dem Weg, bis Sie sie brauchen.

## Voraussetzungen

Windows 10 (Version 2004) oder neuer · x64

## Lizenz

- `crates/**` — **MIT OR Apache-2.0** (Rust-Doppellizenz)
- `app/**` — **GPL-3.0-or-later** (+ unRAR exception)

Einzelheiten in [LICENSING.md](../LICENSING.md).

## Hinweise zu Drittanbietern

Siehe [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md). Insbesondere **unrar** (© Alexander Roshal) wird ausschließlich zum Entpacken von RAR verwendet — OtterZip erstellt niemals RAR-Archive.

## Datenschutz

Keine Werbung, keine Konten, kein Tracking. Absturzberichte sind opt-in und standardmäßig aus. Siehe [PRIVACY.md](../PRIVACY.md).

---

<div align="center">

© 2026 LumiBear Studio

</div>
