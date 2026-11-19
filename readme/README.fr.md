<div align="center">

<img src="images/otter.png" width="96" alt="OtterZip">

# OtterZip

**L'outil d'archivage discret pour Windows.**

Un clic droit pour compresser ou extraire. Sans publicité, sans compte, sans suivi.

[**Site web**](https://lumibear.dev/apps/otterzip/) · [**Microsoft Store**](https://apps.microsoft.com/detail/9NWQNGGSWJCL) · [**Télécharger**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)

[English](../README.md) · [한국어](README.ko.md) · [日本語](README.ja.md) · [中文](README.zh.md) · [Deutsch](README.de.md) · Français · [Español](README.es.md) · [Português](README.pt.md) · [Русский](README.ru.md) · [Italiano](README.it.md)

</div>

<br>

<img src="images/window.png" alt="OtterZip main window">

## Zippé. Fini.

Clic droit sur un fichier — il est zippé. Clic droit sur une archive — elle est sortie. Vous ouvrez à peine l'application.

## Pourquoi OtterZip

- **Directement depuis l'Explorateur** — compressez et extrayez depuis le menu contextuel. Aucune fenêtre à ouvrir d'abord.
- **Une seule chose, bien faite** — pas de modes, pas d'encombrement, rien à apprendre.
- **Sans publicité. Sans compte. Sans suivi.** Aucun logiciel additionnel, aucune relance, aucune inscription. Les rapports d'incident sont strictement facultatifs.
- **Un cœur natif, discrètement rapide** — le moteur est écrit en Rust. Il fait le travail et ne vous laisse pas attendre.
- **L'allure de Windows, parce que c'en est** — conçu avec C# et WinUI 3. Une véritable interface native, pas une page web dans une fenêtre.
- **À votre main** — clair, sombre ou selon le système. Dix langues intégrées.

## Installation

### Microsoft Store — recommandé

Installation en un clic, et les mises à jour arrivent automatiquement.

[Obtenir depuis le Microsoft Store](https://apps.microsoft.com/detail/9NWQNGGSWJCL)

### Téléchargement direct — gratuit

1. Téléchargez [**OtterZip_x64_installer.zip**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)
2. Extrayez le zip.
3. Clic droit sur `Install.ps1` → **Exécuter avec PowerShell**, puis acceptez l'invite.

Le paquet est signé par LumiBear Studio plutôt que par le Store : la première installation enregistre donc notre certificat d'éditeur, une seule fois — c'est à cela que sert l'invite. Guide pas à pas : [**Comment installer**](https://lumibear.dev/apps/otterzip/#install)

Les deux canaux proposent exactement la même application.

## Formats

**Créer** — ZIP · 7z · TAR · TAR.GZ

**Extraire** — ZIP · 7z · RAR · TAR · TAR.GZ · GZ · BZ2 · XZ · ZST · LZ4 · ISO · CAB · JAR · APK · IPA et plus

Chiffrement AES-256 et archives fractionnées (multi-volumes) quand vous en avez besoin.

## Des réglages sensés, dès le départ

<img src="images/settings.png" width="620" alt="OtterZip settings">

Thème, langue, règles de remplacement et plus encore — réglés comme vous le souhaitez, et hors de vue jusqu'à ce que vous en ayez besoin.

## Configuration requise

Windows 10 (version 2004) ou ultérieur · x64

## Licence

- `crates/**` — **MIT OR Apache-2.0** (double licence Rust)
- `app/**` — **GPL-3.0-or-later** (+ unRAR exception)

Voir [LICENSING.md](../LICENSING.md) pour les détails.

## Mentions de tiers

Voir [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md). En particulier, **unrar** (© Alexander Roshal) sert uniquement à l'extraction RAR — OtterZip ne crée jamais d'archives RAR.

## Confidentialité

Sans publicité, sans compte, sans suivi. Le rapport d'incident est facultatif et désactivé par défaut. Voir [PRIVACY.md](../PRIVACY.md).

---

<div align="center">

© 2026 LumiBear Studio

</div>
