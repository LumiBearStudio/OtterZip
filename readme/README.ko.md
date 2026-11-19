<div align="center">

<img src="images/otter.png" width="96" alt="OtterZip">

# OtterZip

**Windows를 위한 조용한 압축 도구.**

우클릭으로 압축하고, 우클릭으로 풉니다. 광고 없음, 계정 없음, 추적 없음.

[**웹사이트**](https://lumibear.dev/ko/apps/otterzip/) · [**Microsoft Store**](https://apps.microsoft.com/detail/9NWQNGGSWJCL) · [**다운로드**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)

[English](../README.md) · 한국어 · [日本語](README.ja.md) · [中文](README.zh.md) · [Deutsch](README.de.md) · [Français](README.fr.md) · [Español](README.es.md) · [Português](README.pt.md) · [Русский](README.ru.md) · [Italiano](README.it.md)

</div>

<br>

<img src="images/window.png" alt="OtterZip main window">

## 압축. 끝.

파일을 우클릭하면 압축됩니다. 압축 파일을 우클릭하면 풀립니다. 앱을 열 일이 거의 없습니다.

## OtterZip을 쓰는 이유

- **탐색기에서 바로** — 우클릭 메뉴에서 곧장 압축하고 풉니다. 창을 먼저 열 필요가 없습니다.
- **한 가지를 제대로** — 모드도, 군더더기도, 배울 것도 없습니다.
- **광고 없음. 계정 없음. 추적 없음.** 끼워팔기도, 성가신 안내도, 가입할 것도 없습니다. 크래시 리포트는 철저히 옵트인입니다.
- **네이티브 코어, 조용히 빠르게** — 엔진은 Rust로 작성했습니다. 할 일을 하고, 기다리게 두지 않습니다.
- **Windows처럼 보이는 이유는, 실제로 그렇기 때문** — C#과 WinUI 3으로 만들었습니다. 창 안에 띄운 웹페이지가 아니라 진짜 네이티브 인터페이스입니다.
- **원하는 대로** — 라이트, 다크, 또는 시스템 설정 따르기. 10개 언어 내장.

## 설치

### Microsoft Store — 권장

한 번의 클릭으로 설치되고, 업데이트는 자동으로 들어옵니다.

[Microsoft Store에서 받기](https://apps.microsoft.com/detail/9NWQNGGSWJCL)

### 직접 다운로드 — 무료

1. [**OtterZip_x64_installer.zip**](https://github.com/LumiBearStudio/OtterZip/releases/latest/download/OtterZip_x64_installer.zip)을 내려받습니다.
2. zip을 풉니다.
3. `Install.ps1`을 우클릭 → **PowerShell에서 실행**, 그리고 표시되는 확인 창을 수락합니다.

이 번들은 Store가 아니라 LumiBear Studio가 서명했기 때문에, 첫 설치 때 게시자 인증서를 한 번 등록합니다 — 확인 창은 그 때문입니다. 단계별 안내: [**설치 방법**](https://lumibear.dev/ko/apps/otterzip/#install)

두 경로 모두 완전히 같은 앱입니다.

## 포맷

**생성** — ZIP · 7z · TAR · TAR.GZ

**추출** — ZIP · 7z · RAR · TAR · TAR.GZ · GZ · BZ2 · XZ · ZST · LZ4 · ISO · CAB · JAR · APK · IPA 등

필요할 때는 AES-256 암호화와 분할(멀티볼륨) 압축 파일도 지원합니다.

## 기본값부터 합리적으로

<img src="images/settings.png" width="620" alt="OtterZip settings">

테마, 언어, 덮어쓰기 규칙 등 — 원하는 대로 맞추고, 필요할 때까지는 눈에 띄지 않습니다.

## 요구 사항

Windows 10 (버전 2004) 이상 · x64

## 라이선스

- `crates/**` — **MIT OR Apache-2.0** (Rust 듀얼 라이선스)
- `app/**` — **GPL-3.0-or-later** (+ unRAR exception)

자세한 내용은 [LICENSING.md](../LICENSING.md)를 참고하십시오.

## 서드파티 고지

[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md)를 참고하십시오. 특히 **unrar** (© Alexander Roshal)는 RAR 추출에만 사용하며 — OtterZip은 RAR 압축 파일을 생성하지 않습니다.

## 개인정보 보호

광고 없음, 계정 없음, 추적 없음. 크래시 리포트는 옵트인이며 기본값은 꺼짐입니다. [PRIVACY.md](../PRIVACY.md)를 참고하십시오.

---

<div align="center">

© 2026 LumiBear Studio

</div>
