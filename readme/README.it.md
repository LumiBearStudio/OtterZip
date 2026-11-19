<div align="center">

<img src="images/otter.png" width="96" alt="OtterZip">

# OtterZip

**Lo strumento per archivi discreto per Windows.**

Clic destro per comprimere o estrarre. Niente pubblicità, niente account, nessun tracciamento.

[**Sito web**](https://lumibear.dev/apps/otterzip/) · [**Microsoft Store**](https://apps.microsoft.com/detail/9NWQNGGSWJCL) · [**Download**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)

[English](../README.md) · [한국어](README.ko.md) · [日本語](README.ja.md) · [中文](README.zh.md) · [Deutsch](README.de.md) · [Français](README.fr.md) · [Español](README.es.md) · [Português](README.pt.md) · [Русский](README.ru.md) · Italiano

</div>

<br>

<img src="images/window.png" alt="OtterZip main window">

## Zippa. Fatto.

Clic destro su un file: è compresso. Clic destro su un archivio: è estratto. L'app quasi non la apri.

## Perché OtterZip

- **Direttamente da Esplora file** — comprimi ed estrai dal menu contestuale. Nessuna finestra da aprire prima.
- **Una cosa sola, fatta bene** — niente modalità, niente ingombri, niente da imparare.
- **Niente pubblicità. Niente account. Nessun tracciamento.** Nessun software aggiuntivo, nessun avviso insistente, nessuna registrazione. I rapporti di arresto anomalo sono inviati solo su tua scelta esplicita.
- **Un core nativo, veloce senza clamore** — il motore è scritto in Rust. Fa il suo lavoro e non ti lascia ad aspettare.
- **Sembra Windows, perché lo è** — realizzato con C# e WinUI 3. Un'interfaccia nativa vera, non una pagina web in una finestra.
- **Regolabile a tuo modo** — chiaro, scuro o come il sistema. Dieci lingue integrate.

## Installazione

### Microsoft Store — consigliato

Installazione con un clic e aggiornamenti automatici.

[Scarica dal Microsoft Store](https://apps.microsoft.com/detail/9NWQNGGSWJCL)

### Download diretto — gratuito

1. Scarica [**OtterZip_x64_installer.zip**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)
2. Estrai il file zip.
3. Clic destro su `Install.ps1` → **Esegui con PowerShell**, quindi accetta la richiesta.

Il pacchetto è firmato da LumiBear Studio anziché dallo Store, quindi la prima installazione registra una sola volta il nostro certificato di autore: è a questo che serve la richiesta. Guida passo passo: [**Come installare**](https://lumibear.dev/apps/otterzip/#install)

I due canali offrono esattamente la stessa app.

## Formati

**Creazione** — ZIP · 7z · TAR · TAR.GZ

**Estrazione** — ZIP · 7z · RAR · TAR · TAR.GZ · GZ · BZ2 · XZ · ZST · LZ4 · ISO · CAB · JAR · APK · IPA e altri

Crittografia AES-256 e archivi divisi (multi-volume) quando servono.

## Impostazioni predefinite sensate, da subito

<img src="images/settings.png" width="620" alt="OtterZip settings">

Tema, lingua, regole di sovrascrittura e altro: configurabili come preferisci e fuori dai piedi finché non servono.

## Requisiti

Windows 10 (versione 2004) o successivo · x64

## Licenza

- `crates/**` — **MIT OR Apache-2.0** (doppia licenza Rust)
- `app/**` — **GPL-3.0-or-later** (+ unRAR exception)

Per i dettagli vedi [LICENSING.md](../LICENSING.md).

## Note su software di terze parti

Vedi [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md). In particolare **unrar** (© Alexander Roshal) è usato solo per l'estrazione RAR: OtterZip non crea mai archivi RAR.

## Privacy

Niente pubblicità, niente account, nessun tracciamento. La segnalazione degli arresti anomali è facoltativa e disattivata per impostazione predefinita. Vedi [PRIVACY.md](../PRIVACY.md).

---

<div align="center">

© 2026 LumiBear Studio

</div>
