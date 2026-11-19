<div align="center">

<img src="images/otter.png" width="96" alt="OtterZip">

# OtterZip

**Тихий архиватор для Windows.**

Правый клик — сжать или распаковать. Без рекламы, без аккаунтов, без слежки.

[**Сайт**](https://lumibear.dev/apps/otterzip/) · [**Microsoft Store**](https://apps.microsoft.com/detail/9NWQNGGSWJCL) · [**Скачать**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)

[English](../README.md) · [한국어](README.ko.md) · [日本語](README.ja.md) · [中文](README.zh.md) · [Deutsch](README.de.md) · [Français](README.fr.md) · [Español](README.es.md) · [Português](README.pt.md) · Русский · [Italiano](README.it.md)

</div>

<br>

<img src="images/window.png" alt="OtterZip main window">

## Сжал. Готово.

Правый клик по файлу — он в архиве. Правый клик по архиву — он распакован. Само приложение вы почти не открываете.

## Почему OtterZip

- **Прямо из проводника** — сжатие и распаковка прямо из контекстного меню. Ничего не нужно открывать заранее.
- **Одно дело, сделанное хорошо** — без режимов, без лишнего, без обучения.
- **Без рекламы. Без аккаунтов. Без слежки.** Ничего не устанавливается в довесок, ничего не выпрашивается, регистрация не нужна. Отчёты о сбоях — строго по вашему согласию.
- **Нативное ядро, спокойно быстрое** — движок написан на Rust. Он делает работу и не заставляет ждать.
- **Выглядит как Windows, потому что это Windows** — собрано на C# и WinUI 3. Настоящий нативный интерфейс, а не веб-страница в окне.
- **Настраивается под вас** — светлая тема, тёмная или как в системе. Десять языков внутри.

## Установка

### Microsoft Store — рекомендуется

Установка в один клик, обновления приходят автоматически.

[Загрузить из Microsoft Store](https://apps.microsoft.com/detail/9NWQNGGSWJCL)

### Прямая загрузка — бесплатно

1. Скачайте [**OtterZip_x64_installer.zip**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)
2. Распакуйте архив.
3. Правый клик по `Install.ps1` → **Выполнить с помощью PowerShell**, затем подтвердите запрос.

Пакет подписан LumiBear Studio, а не Store, поэтому при первой установке один раз регистрируется наш сертификат издателя — именно для этого нужен запрос. Пошаговое руководство: [**Как установить**](https://lumibear.dev/apps/otterzip/#install)

В обоих каналах — одно и то же приложение.

## Форматы

**Создание** — ZIP · 7z · TAR · TAR.GZ

**Распаковка** — ZIP · 7z · RAR · TAR · TAR.GZ · GZ · BZ2 · XZ · ZST · LZ4 · ISO · CAB · JAR · APK · IPA и другие

Шифрование AES-256 и многотомные архивы — когда они нужны.

## Разумные настройки по умолчанию

<img src="images/settings.png" width="620" alt="OtterZip settings">

Тема, язык, правила перезаписи и остальное — настраивается как вам угодно и не мешает, пока не понадобится.

## Требования

Windows 10 (версия 2004) или новее · x64

## Лицензия

- `crates/**` — **MIT OR Apache-2.0** (двойная лицензия Rust)
- `app/**` — **GPL-3.0-or-later** (+ unRAR exception)

Подробности — в [LICENSING.md](../LICENSING.md).

## Уведомления о стороннем коде

См. [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md). В частности, **unrar** (© Alexander Roshal) используется только для распаковки RAR — OtterZip не создаёт архивы RAR.

## Конфиденциальность

Без рекламы, без аккаунтов, без слежки. Отчёты о сбоях отправляются только с вашего согласия и по умолчанию отключены. См. [PRIVACY.md](../PRIVACY.md).

---

<div align="center">

© 2026 LumiBear Studio

</div>
