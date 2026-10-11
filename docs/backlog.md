# Backlog

작업 기록입니다. 이미 측정으로 확정한 사실과, 아직 남은 일을 나눠 적었습니다.
문서가 영어로 된 다른 파일들과 섞이지만, 이 문서는 작업용이라 한국어로 씁니다.

마지막 갱신: 2026-10-09. 로그인 흐름 다섯 단계(네트워크, JS, 입력, 클릭,
자격증명)를 모두 마쳤고, 하위 리소스를 병렬로 받게 했다(M2.13). `Network.*`
이벤트를 낸다(M3.1). 스크린샷을 그린다(M1.5), 이미지와 그라디언트까지. 실시간으로
보면서 조작할 수 있다(`scripts/watch.sh`, M3.6). ES 모듈이 돈다(M2.11). 스크롤된다(M1.12). 콘솔과 오류를 낸다(M3.7). id 전역 이름(M2.15)과 접근성 트리의 `disabled`. EUC-KR 등 모든 인코딩(M1.9). `innerText`가 보이는 글자다(M2.16). 다음 후보는 §0.4.

## 0. 다음 세션 시작하기

### 0.1 지금 어디인가

`--engine html`은 실제 브라우저 엔진이다. HTTP(S)로 가져오고(TLS는 게스트 안),
HTML을 파싱하고, 페이지 JS를 QuickJS로 돌리고, 스타일시트를 적용하고, 배치하고,
그 결과로 CDP에 답한다. agent-browser로 읽고, 채우고, 클릭하고, 제출할 수 있다.
세션은 메모리 전용 쿠키 저장소에 남는다. 자격증명은 자리표시자로 입력하고
클라이언트에게 돌아가지 않는다(`docs/credentials.md`).

참조 사이트(`https://login.example.com`, React SPA)의 로그인 화면이 네이티브 약
3.5초, WASIX 약 4.2초에 열리고, 800×600에서 Chrome과 1px 안팎으로 같은 자리에 배치되며, `fill`·`click`·
`press Enter`가 된다. 가짜 계정으로 실패 응답까지 확인했다. 실제 계정 로그인은
사용자가 `--secret`으로 직접 한다(README의 "Running it").

### 0.2 환경 준비 (재부팅하면 다시 해야 함)

검증에 쓰는 도구 두 개는 전역 설치가 너무 오래되어 `/tmp`에 따로 받아 썼다.
`/tmp`는 재부팅하면 사라진다.

```bash
npm install --prefix /tmp/ab-0.38.2 agent-browser@0.38.2        # 전역은 0.16.3, 쓸 수 없음
tag=$(curl -s https://api.github.com/repos/wasmerio/wasmer/releases/latest | python3 -c 'import sys,json;print(json.load(sys.stdin)["tag_name"])')
mkdir -p /tmp/wasmer-new && curl -sSL "https://github.com/wasmerio/wasmer/releases/download/$tag/wasmer-darwin-arm64.tar.gz" | tar -xz -C /tmp/wasmer-new
export PATH=/tmp/ab-0.38.2/node_modules/.bin:/tmp/wasmer-new/bin:$PATH   # 전역 wasmer는 4.2.6, 쓸 수 없음
```

`cargo-wasix`와 `~/.wasixcc`는 설치되어 있다. Chrome 비교용 headless shell은
`~/Library/Caches/ms-playwright/chromium_headless_shell-1228/`에 있다(§6).

### 0.3 확인 명령

모두 통과하는 상태에서 세션을 끝냈다.

| 명령 | 검사 수 |
|---|---|
| `cargo test` | 엔진 202, browser-core 1, cdp-wire 3 |
| `./scripts/verify-m0.sh`, `MODE=wasix ./scripts/verify-m0.sh` | 16 |
| `./scripts/verify-m1.sh` | 42 |
| `./scripts/verify-login.sh`, `MODE=wasix ./scripts/verify-login.sh` | 33 |
| `WAIT_FOR='#first-field' ./scripts/compare-layout.sh https://login.example.com/` | Chrome과 비교. 13개 중 8개가 2px 안, 평균 1.4px |
| `WHB_PROBE_URL=https://login.example.com/ cargo test --release -p engine probe_a_url -- --ignored --nocapture` | 오류 없이 `#/login`까지 |

WASIX 모드를 연달아 돌리면 포트 재사용 때문에 약 30초 기다린다(§6).

### 0.4 다음 후보

- **참조 사이트 로고.** `div.logo`의 `<img>`가 우리 엔진에서는 `src` 없이 남는다.
  Chrome에서는 보인다. 로고 주소를 정하는 스크립트 경로를 `probe_a_url`로 찾아야 한다.
- **WebAssembly.** QuickJS에 없다. vite.dev의 코드 하이라이터(oniguruma)가 이것 때문에
  실패한다. 인터프리터 안에서 wasm을 돌리는 것은 범위 밖으로 본다.
- **그림자, SVG.** `box-shadow`와 SVG 이미지(`<img src=*.svg>`, 인라인 `<svg>`)는 그리지 않는다.
- **요청 가로채기 (M3.3).** agent-browser `network route`. `Fetch.requestPaused`는
  엔진이 요청 도중 클라이언트의 답을 기다려야 해서 구조가 바뀐다(§4.4).

### 0.5 이번 세션의 커밋 (오래된 것부터)

| 커밋 | 내용 |
|---|---|
| `7794b8e` | BL-010: M2 엔진 연결 검증. `textContent` setter, sloppy 평가, 락파일 정리 |
| `9ecfa0e` | HTTPS: rustls + ring을 게스트 안에서 (M1.11) |
| `84e6ff8` | 쿠키 저장소, POST, 히스토리, 문서마다 새 JS 컨텍스트 (M3.2) |
| `c30f103` | DOM 프렐류드 재작성(`prelude/*.js`), 외부 스크립트, 이벤트 루프, fetch/XHR (M2.6·M2.7·M2.9) |
| `77494d7` | QuickJS 패치(`Function.caller`), 스택 32MB. 참조 사이트 로그인 화면이 뜸 (M2.10) |
| `4862ea2` | 원격 객체, 글자·키 입력, keep-alive 연결 풀, `WHB_TRACE_CDP` (M2.4·M2.8·M2.14) |
| `10820d2` | 추적 로그에서 입력 글자 가리기 |
| `f2771cc` | 인라인 스타일로 숨긴 요소를 접근성 트리에서 제외 (M1.4a) |
| `8f1ad7f` | 선택자 엔진 재작성 (M1.10) |
| `bffaf73` | CSS 파서와 캐스케이드 계산 (M1.4b) |
| `0cf68cb` | 캐스케이드를 페이지에 연결: 시트 수집, 캐시, 접근성 트리, `getComputedStyle` |
| `8abfeaa` | 레이아웃(taffy), 히트 테스트, 마우스 입력 (M1.4·M2.5) |
| `d9c4296` | 자격증명: stdin, 자리표시자, CDP 출력 가리기 (M4) |
| `ee1a18c` | 백로그 정리 |
| `f7b8e6c` | 하위 리소스 병렬 다운로드 (M2.13) |
| `a38c09f` | `Network.*` 이벤트, 인코딩된 비밀 가리기 (M3.1) |
| `f113590` | 스크린샷, 내장 한글 글꼴, 레이아웃이 실제 글자 폭 사용 (M1.5) |
| `bbcdca2` | 이미지 로딩·디코딩, 배경 이미지, 그라디언트 |

## 1. 엔진 구조

`crates/engine/src/`:

| 파일 | 하는 일 |
|---|---|
| `browser.rs` | `HtmlEngine`. CDP 메서드, 로드 순서(가져오기 → 스타일시트 → 스크립트), 히스토리. 맨 바깥 `impl Engine`이 모든 출력에서 비밀을 가린다 |
| `fetch.rs` | HTTP/1.1 클라이언트. TLS, keep-alive 풀, 리다이렉트, 쿠키. `start`(스레드에서 요청, 호스트당 6개)와 `preload` |
| `network.rs` | `Network.*` 기록. 요청마다 이벤트와 응답 본문을 남기고 엔진이 출구에서 내보낸다 |
| `cookies.rs` | RFC 6265 쿠키 저장소. 메모리 전용, 모든 타깃이 공유 |
| `dom.rs`, `parse.rs` | 아레나 DOM과 html5ever 싱크. 폼 상태(`Control`), 변경 버전, 스타일·레이아웃 캐시 |
| `select.rs` | 선택자 파서·매처·명시도 |
| `css.rs`, `style.rs` | 시트 파싱, 캐스케이드, 계산 스타일 (`document_styles`) |
| `layout.rs` | taffy 위의 배치, 칠하는 순서, 히트 테스트 (`document_layout`) |
| `font.rs` | 내장 글꼴(`fonts/NotoSansKR-subset.otf`). 레이아웃의 글자 폭과 그리기의 글리프가 여기서 나온다 |
| `paint.rs`, `color.rs`, `png.rs` | 스크린샷: 레이아웃을 칠하고 PNG로. 배경 이미지와 그라디언트 해석도 `paint.rs` |
| `images.rs` | 탭의 이미지 저장소: 가져오기, 크기, 디코딩(PNG·JPEG·GIF·WebP), 기다리기 |
| `screencast.rs` | `Page.startScreencast`: 바뀐 화면을 JPEG 프레임으로, 응답을 받은 뒤에만 |
| `scroll.rs` | 스크롤 위치(문서에 저장), 범위, `scrollIntoView` 정렬, 휠 |
| `text.rs` | `innerText`: 계산 스타일로 보이는 글자를 모으고 공백을 접고 줄을 나눈다 |
| `encoding.rs` | 문자 인코딩: 문서 인코딩 판정(BOM·헤더·meta·추정), 응답 디코딩, `TextDecoder` |
| `modules.rs` | ES 모듈의 지정자 해석(import map 포함)과 로딩. QuickJS가 링크와 평가를 맡는다 |
| `ax.rs` | 접근성 트리 |
| `js.rs` | `ScriptHost`: QuickJS를 전용 스레드(32MB 스택)에 가두고 채널로 부른다. 가상 시계 작업 큐, CDP용 평가 |
| `bindings.rs` | JS에 주는 원시 함수(`__dom`)와 `PRELUDE` 조립 |
| `prelude/*.js` | JS 쪽 DOM과 웹 플랫폼. 번호 순서로 이어 붙여 함수 하나가 된다. `90-cdp.js`는 원격 객체와 입력 |
| `secrets.rs` | 자격증명 보관, 자리표시자 치환, 출력 가리기 |
| `test_server.rs` | 테스트용 HTTP 서버 |

설계상 알아둘 것:

- `rquickjs::Runtime`은 `Send`가 아니다. 그래서 **전용 스레드에 가두고 채널로 통신**한다
  (`ScriptHost`). 엔진은 `Sender`만 들고 있어 `Engine: Send`가 유지된다.
- DOM은 `Arc<Mutex<Document>>`로 엔진과 공유한다. **JS를 부르는 동안 문서 잠금을
  잡고 있으면 교착한다.** `replace_document` 류가 잠금 안에서 값을 모았다가 놓은
  뒤 대입하는 이유다.
- 스타일과 레이아웃은 문서에 캐시되고 `Document::version`이 바뀔 때만 다시 계산된다.
  노드를 바꾸는 `dom.rs`의 모든 함수가 버전을 올린다. 새 변경 함수를 만들면 `touch()`를 부른다.
- CDP 출력은 `impl Engine for HtmlEngine` 한 곳을 지난다. 새 메서드는 `dispatch_raw`에
  넣으면 가리기가 저절로 적용된다.
- 원시 접근자는 매크로로 만들면 매크로 위생 때문에 실패한다. 그래서 `bindings.rs`의
  `put_*` 함수는 명시적으로 쓴다.

## 2. 측정으로 확정한 사실

다시 논의하지 않아도 되는 것들입니다. 근거는 각 문서에 있습니다.

| 사실 | 근거 |
|---|---|
| 클라이언트가 실제로 쓰는 CDP는 메서드 24개, 이벤트 25개다 | [docs/cdp-surface.md](cdp-surface.md) |
| 클릭 자동화가 크롤링보다 훨씬 비싸다. `getComputedStyle`, `getBoundingClientRect`, `elementFromPoint`를 페이지 JS로 돌린다 | 같은 문서 |
| WASIX에서 loopback bind·connect·스레드가 모두 동작한다 | [docs/wasix.md](wasix.md) |
| 소켓 거부는 `EOPNOTSUPP`로 나타나며, 이는 WASIX의 한계가 아니라 런타임 정책이다. Wasmer에서는 `--net`이 필요하다 | 같은 문서 |
| 브라우저 샌드박스 안에서도 loopback이 동작한다. 엔진과 클라이언트를 한 샌드박스에 함께 둘 수 있다 (E1) | 같은 문서 |
| 호스트 페이지에서 안으로 들어오는 경로(E3)는 필요 없다. 설령 되더라도 HTTP 전용이라 CDP의 WebSocket 업그레이드는 지나가지 못한다 | [harness/README.md](../harness/README.md) |
| 브라우저 샌드박스에서 외부 TCP·HTTPS가 된다. WISP 릴레이를 쓴다 (E4·E5) | 이 문서의 §3 |
| QuickJS는 WASIX에서 실행된다. `rquickjs` 0.14 + `bindgen`, 컨텍스트는 `Context::custom`으로 만든다. 엔진에서는 `full-wasi`를 쓸 수 없다 (§6) | [docs/quickjs.md](quickjs.md) |
| WASIX 모듈 크기: QuickJS 이전 272KB, QuickJS 1.6MB, TLS 2.3MB, 레이아웃까지 3.3MB, 글꼴과 래스터라이저까지 4.7MB, 이미지 디코더까지 5.2MB. 앞의 몇 개는 빌드 스크립트 표시값이라 실제보다 클 수 있다 | `ls -l`, 커밋 기록 |
| 참조 사이트 배경은 인라인 `background-image`(`/api/login-step`, 1920×1080 JPEG 766KB)이고, 크기·위치는 `.login-background`의 `background: ... no-repeat top; background-size: 1920px 1080px`에서 온다. `cover`를 쓰는 규칙은 `min-height:1081px` 미디어 쿼리 안이라 800×600에서는 안 맞는다 | 2026-10-10 Chrome과 비교 |
| 참조 사이트는 `Noto Sans KR, ...`으로 Noto Sans KR 웹 글꼴을 쓴다. 왼쪽 절반은 화면 전체에 깔린 배경 이미지와 그 위의 흰 글자이고, 로그인 상자는 그 위의 80% 흰색이다 | 2026-10-09 CSS와 Chrome 스크린샷 |
| 참조 사이트 로그인 상자는 오른쪽에서 미끄러져 들어온다. Chrome에서 열자마자 재면 x가 수십 px 다르다 | 같은 날 |
| WASIX에서 rustls + ring이 TLS 1.3으로 동작한다. 참조 사이트 인증서는 공개 루트로 검증된다 | [docs/wasix.md](wasix.md) |
| example.com은 이제 본문이 문단 하나와 `<script src=/s.js>`뿐이다. 외부 스크립트 없이는 제목과 링크가 없다 | 2026-10-09 측정 |
| 참조 사이트 로그인 화면을 800×600에서 Chrome(Playwright headless shell)과 비교하면 입력란·`.login-form`·`#wrap`·저작권 문구의 위치와 크기가 같거나 1px 차이다. 체크박스만 x가 8px 다르다. 아이디 입력란 클릭은 포커스를, "체크박스" 글자 클릭은 체크를 두 엔진에서 똑같이 바꾸고, 숨은 체크박스를 ref로 누르면 두 엔진 모두 빗나간다(`div.login-background`가 맞는다) | 2026-10-09 |
| 캐스케이드를 넣은 뒤 참조 사이트 "다음" 버튼의 계산 스타일은 `display: block`, `cursor: pointer`, `width: 100%`, `height: 54px`, 배경 `#00aaff`(`a CSS variable`에서), 13.33px이다. 전체 스냅샷은 23줄이다 | 2026-10-09 |
| 참조 사이트의 숨은 화면(추가 인증 선택, OTP, 실패 대화상자)은 CSS 클래스가 아니라 인라인 `style="display: none"`으로 숨는다. 인라인 스타일만 반영해도 로그인 화면의 인터랙티브 스냅샷이 25개 넘던 것에서 4개(첫 입력란, 아이디, 다음, 체크박스)로 준다 | 2026-10-09 측정 (M1.6) |
| 참조 사이트에 가짜 계정으로 로그인하면 실패가 화면에 나온다. Enter 한 번에 아이디 단계(`POST /api/login-step`, a result code), 버튼이 "로그인"으로 바뀐 뒤 Enter 한 번 더에 인증(a result code "실패 문구")이고, 그 문구가 폼 아래 문단과 확인 대화상자로 스냅샷에 나온다. 클릭 없이 Enter만으로 된다 | 2026-10-09, 사용자 승인 아래 가짜 계정 `nonexistent-test-account`로 |
| 참조 사이트의 React 입력란에 agent-browser `fill`이 동작한다. 아이디는 스냅샷에 나오고 비밀번호는 나오지 않는다 | 2026-10-09 agent-browser |
| 참조 사이트의 로그인 화면이 WASIX에서 그려진다. 숨은 화면을 빼면 첫 단계 스냅샷은 첫 입력란·아이디·"다음"·체크박스뿐이고, 비밀번호 칸은 "다음" 뒤에 나온다 | 2026-10-09 agent-browser |
| QuickJS 호출은 V8보다 네이티브 스택을 훨씬 많이 쓴다. 기본 한도 1MB로는 React 렌더링이 넘친다. 스크립트 스레드는 32MB 스택에 한도 24MB로 돈다 | 같은 측정 |
| 참조 사이트는 gzip을 요청마다 즉석에서 압축해서 오히려 느리다. The largest bundle는 원본 4.6MB가 0.37~0.49초, gzip 1.2MB가 0.84~0.90초(curl, 세 번씩). 그래서 엔진은 `Accept-Encoding: identity`를 유지한다. 느린 회선에서는 반대일 수 있다 | 2026-10-09 |
| 엔진 클라이언트의 다운로드 속도는 curl과 같다(큰 파일 4개 11MB를 순차 0.9~1.2초, 병렬 0.7~0.9초). 이 회선에서 병렬은 처리량보다 파싱과 겹치는 데 쓸모가 있다 | 같은 날 |
| 참조 사이트는 React 번들 3개와 the grid library·jQuery·lottie를 싣는다. jQuery는 초기화 때 `createHTMLDocument`를, lottie는 캔버스 2D 컨텍스트를 요구하고, the grid library는 `Function.caller`를 요구한다 | 2026-10-09 `probe_a_url` |
| agent-browser의 `back`·`forward`는 CDP 메서드가 아니라 `Runtime.evaluate("history.back()")`이다 | [docs/capture/required-expressions.txt](capture/required-expressions.txt) |
| `Context::full`이 죽는 원인은 sysroot 불일치 하나다. `CLOCK_MONOTONIC`이 포인터 인코딩(1058440)으로 컴파일되고 WASIX는 정수 1을 기대한다 | 같은 문서 |
| 참조 사이트 사이트는 SPA 셸만 돌려준다. HTML만 받아서는 아무것도 얻을 수 없다 | 이 문서의 §3 |

## 3. 브라우저 샌드박스의 외부 통신 (E4·E5)

`harness`에 아웃바운드 프로브 두 개를 추가했습니다.

- **E4** 는 샌드박스에서 임의의 외부 호스트로 TCP 연결을 엽니다. TLS 포트에
  평문 요청을 보내므로, 연결 성립 자체를 판정 신호로 씁니다.
- **E5** 는 샌드박스 안의 `curl/curl` 패키지로 실제 HTTPS 요청을 보냅니다.
  TLS가 게스트 안에서 처리되므로 인증서까지 게스트 책임입니다.

참조 사이트 주소로 돌린 결과입니다.

```
E4 PASS: TCP connected (login.example.com:443)
E5 PASS: HTTP/1.1 200 OK / 3593 bytes
  Content-Length: 3208, Content-Type: text/html, Etag: "6ac5f37d-c88"
```

응답 본문에는 `<title>`이 없고 `<div id="first-loading">`만 있습니다.
로드밸런서의 로딩 오버레이이며, 실제 화면은 `the main stylesheet`와
the grid library 번들을 받은 뒤 JS가 그립니다. **"HTML을 가져온다"와 "페이지를
읽는다"가 다른 일이라는 것을 이 페이지 하나가 증명합니다.**

호스트에서 같은 주소로 curl한 결과와 헤더가 일치합니다. WISP 릴레이가 사용자
머신에서 열린 TCP 연결이므로 송신 IP가 같은 것은 당연합니다.

## 4. 다음 작업

### 4.0 진행 순서: 로그인 흐름 하나를 끝까지

목표는 화면을 읽고, 클릭하고, 아이디와 비밀번호를 넣어 제출하는 것입니다.
마일스톤 순서대로 하지 않고 이 흐름이 요구하는 순서로 합니다.

1. **네트워크 기반.** HTTPS(M1.11), POST와 쿠키 저장소(M3.2). **완료**
2. **JS가 화면을 그린다.** 외부 스크립트(M2.7), 이벤트 루프와 Promise(M2.6),
   `fetch`/XHR(M2.9). **완료.** 참조 사이트의 로그인 화면이 네이티브와 WASIX에서
   그려진다. 열리는 데 3.5초 걸린다(M2.13)
3. **입력과 제출.** `Input.insertText`가 포커스된 입력란을 바꾸고 이벤트를 낸다
   (M2.8). 엔터와 제출 버튼이 폼 제출로 이어진다. `callFunctionOn`을 실제 JS로
   (M2.4). **완료.** `scripts/verify-login.sh`가 로컬 로그인 픽스처로 `fill` →
   `press Enter` → 로그인 후 화면 → 새로고침 뒤 세션 유지까지 네이티브와 WASIX에서
   검사한다(18개). 참조 사이트에서는 `fill`까지 확인했고, 실제 제출은 하지 않았다
4. **클릭.** 실제 화면을 측정(M1.6)한 뒤 레이아웃과 히트테스트(M1.4),
   마우스 이벤트(M2.5). **완료.** 참조 사이트 로그인 화면이 Chrome과 거의 같은
   자리에 배치되고, 클릭 결과도 같다
5. **자격증명.** 비밀번호가 어디로도 새지 않게 한다 (M4). **완료.** 실제 계정으로
   참조 사이트에 로그인하는 것은 사용자가 이 경로로 직접 한다

검증은 로컬 로그인 픽스처로 합니다. 쿠키를 발급하는 작은 서버와 JS로
그려지는 로그인 화면을 두고, agent-browser로 open → fill → click → 로그인 후
화면까지 자동 검사합니다. 참조 사이트는 마지막에 확인합니다.

### 4.1 밀린 정리 (P0, 짧음)

- [x] BL-001 `harness`의 E4·E5 변경과 `wisp-server.mjs`를 커밋한다.
- [x] BL-002 `docs/wasix.md`에 E4·E5 결과를 반영한다.
- [x] BL-003 프로브가 본문을 온전히 출력하도록 고쳐서 판정식을 되돌린다.
      이제 E1 판정은 `webSocketDebuggerUrl`을 직접 본다.
- [x] BL-004 `scripts/verify-harness.sh`를 만들었다. 하니스와 릴레이를 띄우고,
      페이지를 열고, 완료를 폴링하고, 프로브마다 판정한 뒤 스스로 정리한다.
- [x] BL-005 LICENSE 파일을 만들었다 (MIT).
- [x] BL-006 `scripts/harness-stop.sh`를 만들었다. 포트를 점유한 프로세스가
      이 리포의 것인지 확인한 뒤에만 종료한다.
- [ ] BL-007 5001에 있는 정체불명 프로세스를 확인한다. PID 15758,
      `node serve.mjs`, 부모 PID 1이며 "wasmer-test local WISP proxy"라고 응답한다.
      이 리포의 것이 아니다.

### 4.2 M1 실제 엔진 (P0, 큼)

`crates/engine-fake`를 대체할 `crates/engine`을 만듭니다. `browser-core`의
`Engine` trait은 그대로 두고 구현만 갈아끼웁니다.

- [x] M1.1 `html5ever`로 HTML을 파싱해 실제 DOM 트리를 만든다.
      `crates/engine/src/dom.rs`와 `parse.rs`가 아레나 DOM과 `TreeSink`를
      담당한다. `Send`이고 핸들이 색인이라 M2의 JS 바인딩에 그대로 쓴다.
- [x] M1.2 접근성 트리를 실제 DOM 위에서 계산한다.
      `crates/engine/src/ax.rs`. role·name·parentage·backendDOMNodeId를 내고,
      레벨과 컨트롤 값을 붙인다. **비밀번호 필드의 값은 비운다.**
      `engine-fake`가 냈던 로그인 페이지 결과를 그대로 재현하는 테스트가 있다.
- [x] M1.3 엔진이 직접 페이지를 가져온다. `crates/engine/src/fetch.rs`가
      `std::net`으로 평문 HTTP를 열고, 청크 인코딩과 리다이렉트와 `data:` URL을
      처리한다. `std::net`으로 열면 네이티브와 WASIX 양쪽에서 동작하고, 브라우저
      샌드박스에서는 E4·E5로 확인한 대로 WISP가 자동으로 처리한다. **WISP를
      인지하는 코드는 필요 없다.** HTTPS는 M1.11, keep-alive는 M2.13 참조.
- [x] M1.11 HTTPS. rustls + ring + `webpki-roots`로 TLS를 게스트 안에서
      처리한다. 사설 CA는 `WHB_EXTRA_CA_CERTS`로 추가한다. WASIX에서
      example.com과 참조 사이트가 TLS 1.3으로 열린다. 근거는
      [docs/wasix.md](wasix.md)의 "TLS inside the guest".
- [x] M1.9 모든 문자 인코딩(`encoding.rs`, `encoding_rs`: Firefox가 쓰는 WHATWG 구현,
      순수 Rust, WASIX 모듈 +182KB). 문서는 브라우저 순서로 판정한다: BOM, 헤더의 `charset`,
      앞 1024바이트의 `<meta charset>`·`http-equiv`(meta의 UTF-16은 UTF-8로). 선언이 없으면
      UTF-8로 풀리면 UTF-8, 아니면 EUC-KR(한국어 로캘의 Chrome 기본값; Chrome의 바이트
      추정은 하지 않는다). `document.characterSet`이 그 이름을 낸다. 스크립트와 스타일시트는
      헤더 `charset`, 없으면 문서 인코딩. XHR·이미지 외 응답은 헤더, 없으면 UTF-8. `fetch`의
      `text()`와 모듈은 규격대로 늘 UTF-8. `TextDecoder`는 모든 레이블(`ks_c_5601-1987`,
      `utf-16le` 등)을 받고 UTF-8 밖은 엔진이 푼다. `fatal`, `ignoreBOM`은 되고 `stream`은
      호출마다 따로 푼다. 웃긴대학(meta로만 EUC-KR)이 네이티브·WASIX에서 제대로 나온다(전에는
      제목부터 깨졌다). 같은 작업에서 `SVGAnimatedString`을 진짜 클래스로 만들어 전역에
      냈다(vite.dev의 `instanceof SVGAnimatedString`). vite.dev에 남은 오류 중 FloatingVue는
      Chrome에서도 나는 사이트 자체 버그이고, 남은 차이는 WebAssembly뿐이다.
- [x] M1.10 선택자 엔진을 새로 썼다(`select.rs`). 결합자 넷, 속성 연산자 전부와
      `i` 플래그, `:nth-child(An+B of S)`를 포함한 구조 의사 클래스, `:not`·`:is`·
      `:where`·`:has`, 폼 상태 의사 클래스, `:focus`, 명시도. `:hover`처럼 포인터가
      있어야 참인 것은 거짓이고, 의사 요소로 끝나는 선택자는 요소와 맞지 않는다.
      잘못된 선택자는 스크립트에서 SyntaxError다. 캐스케이드가 같은 파서를 쓴다.
- [x] M1.4a 인라인 `style`의 `display: none`·`visibility: hidden`을 접근성 트리가
      반영한다(`style.rs`). `visibility`의 상속은 아직 하위 트리 전체로 근사한다.
- [x] M1.4b CSS 캐스케이드. `css.rs`가 시트를 읽고 `style.rs`가 계산한다. 출처·
      중요도·명시도·순서, 상속, 사용자 정의 속성과 `var()`, `inherit`/`initial`/
      `unset`, `font-size`의 px 계산, 레이아웃에 쓰는 축약 속성. 시트는 `<style>`,
      파서가 넣은 `<link>`(엔진이 스크립트 전에 가져옴), 스크립트가 넣은 `<link>`,
      CSSOM의 `insertRule`에서 온다. 문서 버전이 그대로면 캐시를 쓰고, 시트는 텍스트가
      바뀔 때만 다시 파싱한다. 접근성 트리와 `getComputedStyle`이 이것을 쓴다.
      `:focus` 규칙은 캐스케이드에 들어가지 않는다(포커스가 스크립트 쪽 상태라서).
- [x] M1.4 레이아웃(`layout.rs`). taffy 0.14가 블록·플렉스·그리드·float를 맡고,
      그 위에 근사한다: 인라인 흐름은 줄바꿈하는 행으로, 글자 폭은 글꼴 크기로
      (한글·CJK는 1em), 폼 컨트롤은 Chrome 기본 크기로, `absolute`는 가까운 위치
      지정 조상에, `fixed`는 뷰포트에, `transform`은 이동만, `calc()`는 퍼센트만.
      칠하는 순서는 쌓임 단계·레이어·문서 순서다. 박스와 히트 테스트가 같은
      레이아웃을 써서, 요소가 보고하는 자리를 누르면 그 요소가 눌린다.
      남은 근사: 기준선 정렬 없음, 회전·확대 무시, 쌓임 맥락은 한 단계로 평평하게.
- [x] M1.12 스크롤(`scroll.rs`). 위치는 `Document`에 뷰포트(문서 노드 id)와 스크롤
      컨테이너(`overflow`가 `visible`이 아닌 요소)별로 둔다. 레이아웃은 스크롤을 모르고,
      `Layout::client_rect`가 감싼 컨테이너들과(고정 하위 트리가 아니면) 뷰포트의 위치를
      뺀다. 그래서 스크롤에 레이아웃 비용이 없고, `getBoundingClientRect`·히트 테스트·
      `DOM.getBoxModel`/`getContentQuads`·그리기가 같은 좌표를 본다. JS: `scrollX/Y`,
      창과 요소의 `scroll`/`scrollTo`/`scrollBy`, `scrollTop/Left`, `scrollWidth/Height`,
      `clientWidth/Height`, `scrollIntoView`(`block`·`inline`), `scrollIntoViewIfNeeded`,
      태스크마다 한 번 `scroll` 이벤트(뷰포트는 문서에서 창까지 버블). 휠은 페이지가
      `preventDefault`하지 않으면 포인터 아래에서 그 방향으로 움직일 수 있는 가장 안쪽
      `auto`/`scroll` 컨테이너를, 없으면 뷰포트를 움직인다(루트·body가 `overflow: hidden`이면
      안 움직임). CDP: `DOM.scrollIntoViewIfNeeded`, `Page.getLayoutMetrics`의 `pageX/Y`,
      스크린샷(clip 없으면 현재 뷰포트)과 스크린캐스트 프레임(`scrollOffsetX/Y`, 스크롤하면
      새 프레임). agent-browser의 `scroll down`, 화면 밖 요소 클릭, `screenshot --full`이 된다.
      없는 것: `scroll-behavior: smooth`(늘 즉시), `position: sticky`, 스크롤 스냅, 스크롤바
      (자리를 차지하지 않는다).
- [x] M1.5 스크린샷(`paint.rs`). 레이아웃의 칠하는 순서대로 캔버스 배경(루트,
      없으면 body), 배경색, 둥근 모서리와 변마다 색이 다른 테두리, 글자, 폼 컨트롤
      (꾸미지 않았으면 Chrome 모양, 값·자리표시자·비밀번호 점·체크), 목록 기호,
      불투명도, `overflow` 자르기를 칠한다. 글꼴은 Noto Sans KR을 한글 11,172자와
      라틴·기호로 줄여 넣었다(1.15MB, OFL, `scripts/make-font.sh`). 참조 사이트가
      웹 글꼴로 쓰는 글꼴이기도 하다. **레이아웃도 이 글꼴의 실제 폭을 쓴다**: 그려진
      글자가 보고된 박스에 꼭 맞고, 참조 사이트를 Chrome과 비교한 평균 오차가 3.6px에서
      1.4px로 줄었다(버튼 내용 세로 가운데 정렬도 이때 고침). `clip`·`scale`·
      `captureBeyondViewport`를 받고 PNG만 낸다(다른 형식은 이유를 밝힌 오류).
      `Page.getLayoutMetrics`가 실제 콘텐츠 크기를 내서 `screenshot --full`이 된다.
      `--annotate`는 agent-browser가 페이지에 넣은 라벨을 그대로 그린다. 한 장에 약
      55ms(WASIX 0.2초). 그리지 않는 것: 이미지, 그라디언트, 그림자, 외곽선, 회전·확대,
      다른 글꼴(굵게는 번지게, 기울임은 똑바로). 비밀: 비밀번호 칸은 글자 수만큼 점이고,
      다른 글자는 그리기 전에 가린다. 레이아웃은 실제 값으로 쟀으므로 자리표시자가
      더 길면 그만큼 줄 폭을 늘려 줄을 나눈다(줄을 먼저 나누고 가리면 줄에 걸친
      비밀이 조각으로 샌다). WASIX 모듈은 4.7MB가 됐다(이때 빌드 스크립트가 5.1M으로
      표시했는데 실제 바이트 수와 다르다. 크기는 `ls -l`로 잰다).
      이어서 이미지(`images.rs`): `image` 크레이트로 PNG·JPEG·GIF(첫 장)·WebP를
      디코딩한다. 파서가 찾은 `<img>`는 파싱 직후, 스크립트가 넣거나 `src`를 바꾼 것은
      그 순간 요청하고, 태스크에서 기다려 `load`/`error`를 쏜다. 그래서 window `load`가
      이미지 뒤에 오고 `naturalWidth`·`complete`·`decode()`가 실제 값이다. 레이아웃은
      기다리지 않고 도착한 이미지의 고유 크기를 쓴다(이미지 세대가 레이아웃 캐시 키에
      들어간다). 배경 이미지는 로드 끝에 요청하고 스크린샷이 최대 5초 기다린다. 박스가
      있는 요소의 배경만 받는다. 그리는 것: `<img>`와 `object-fit`, `background-image`의
      크기(`cover`·`contain`·길이)·위치(3·4값 문법 포함)·반복, `linear-gradient`(각도·
      `to`·정지점), `radial-gradient`(farthest-corner 타원 근사), 둥근 모서리 따라 자르기.
      스타일시트의 `url()`은 시트 주소 기준으로 절대화해서 저장한다. 참조 사이트 로그인
      화면이 Chrome과 거의 같게 나온다(배경 사진, 그 위 흰 글자, 반투명 로그인 상자).
      로드 시간은 그대로(약 2.8초). WASIX 모듈 4.7MB → 5.2MB(5,188,554바이트).
- [ ] M1.6 실제 사이트로 다시 측정한다. 참조 사이트 로그인 화면은 레이아웃까지
      Chrome과 비교했다(§2). 로그인 뒤 화면과 CDP 표면 재캡처는 아직이다.
- [x] M1.7 `browser` 바이너리에 `--engine fake|html` 플래그를 넣었다.
      가짜 엔진이 기본값이라 `verify-m0.sh`는 그대로 통과한다.
- [x] M1.8 `scripts/verify-m1.sh`. 픽스처를 HTTP로 서빙하고 접근성 트리, 인라인
      스크립트, `eval`, 히스토리, 쿠키, 클릭과 가시성, 스크린샷(뷰포트·전체·
      `--annotate`)을 40개 검사로 판정한다.

### 4.3 M2 JavaScript (P0, 큼)

- [x] M2.1 QuickJS를 엔진 모듈에 통합한다. `Context::custom` 경로를 쓴다.
      `wasixcc`를 설치하면 `Context::full`로 단순해진다 (§5 참조).
- [x] M2.2 DOM 바인딩을 만든다. `document`, `window`, `querySelector`,
      `getComputedStyle`, `getBoundingClientRect`, `focus`, `scrollIntoView`,
      `elementFromPoint`가 필요하다. 앞의 다섯 개는 `agent-browser`가 실제로
      호출하는 것을 캡처로 확인했다. 지금은 사각형과 `elementFromPoint`가
      레이아웃(M1.4)에서 나온다.
- [x] M2.3 `Runtime.evaluate`가 규칙표가 아니라 진짜 JS를 실행하게 한다.
      `crates/engine`의 규칙표는 삭제했다. `engine-fake/src/eval.rs`는 M0의
      가짜 엔진이 쓰므로 남겨 두었다.
- [x] M2.4 원격 객체 핸들. `Runtime.evaluate`와 `callFunctionOn`이 실제 JS
      객체 핸들을 주고받는다(`prelude/90-cdp.js`). `returnByValue`와
      `awaitPromise`를 지원한다. 기다리는 Promise는 가상 시간으로 페이지를 돌려
      확인하고, 아무것도 그것을 풀 수 없으면 오류로 알린다. `DOM.resolveNode`·
      `describeNode`·`requestNode`·`focus`가 `backendNodeId`와 `objectId`를 받는다.
- [x] M2.14 키보드. `Input.insertText`는 포커스된 입력란의 선택 영역을 바꾸고
      `beforeinput`/`input`을 낸다. 값은 요소에 프레임워크가 단 속성 아래에서
      바뀌므로 React의 값 추적이 알아챈다. `Input.dispatchKeyEvent`는
      keydown/keypress/keyup과 기본 동작을 낸다: 글자 입력, Enter의 암묵적 제출,
      Backspace/Delete, Tab 이동, 화살표·Home/End, Ctrl+A, Space로 버튼 누르기.
      포커스를 잃을 때 값이 바뀌었으면 `change`를 낸다.
- [x] M2.7 외부 스크립트를 실행한다. 파서 삽입 스크립트는 브라우저 순서대로
      (차단 → `defer` → `DOMContentLoaded` → `async` → 작업 큐 → `load`),
      스크립트가 넣은 스크립트는 넣는 순간 실행한다. `innerHTML`로 들어온
      스크립트는 실행하지 않는다. **모듈은 아직 없다.** `nomodule` 대체 스크립트는 돈다.
- [x] M2.9 `fetch`와 `XMLHttpRequest`. 엔진의 `fetch::Client`를 거치므로 쿠키가
      오간다. 알려진 차이: 본문이 UTF-8 문자열로 Rust를 건너므로 바이너리는
      깨지고, CORS 검사를 하지 않는다.
- [x] M2.8 폼 컨트롤 상태. 입력값·체크·선택을 속성과 따로 `dom::Control`에 둔다.
      페이지 스크립트는 비밀번호 값을 읽는다. 로그인 폼을 제출하려면 읽어야 한다.
      접근성 트리와 CDP 설명에는 나오지 않는다.
- [x] M2.10 `Function.prototype.caller`. 참조 사이트의 the grid library가
      `this._super.caller`로 부모 메서드를 찾는데 QuickJS는 늘 `undefined`를 줬다.
      rquickjs-sys를 `vendor/`에 들여와 QuickJS를 패치했다
      ([vendor/README.md](../vendor/README.md)).
- [x] M2.13 참조 사이트를 여는 시간. 처음 12초 중 9초는 요청 69개가 매번 새로 맺는
      TCP·TLS였고, keep-alive 연결 풀로 3.5초가 됐다. 스타일시트를 받게 되면서 다시
      약 5초. 그때 요청 71개(25.7MB)를 하나씩 받는 데 3.5초였다.
      이제 요청은 필요가 생긴 순간 시작하고 결과는 원래 쓰던 자리에서 받는다.
      `Client::start`가 스레드에서 보내고(호스트당 6개, Chrome과 같음), 파서가 찾은
      스타일시트와 클래식 스크립트는 파싱 직후 `preload`한다. 스크립트가 넣은
      `<script src>`·`<link rel=stylesheet>`, 비동기 `fetch`·XHR은 호출하는 순간
      시작하고 그 태스크에서 결과를 받는다. 그래서 실행·이벤트 순서는 그대로다.
      번갈아 잰 중앙값: 네이티브 4.66초 → 3.56초, WASIX 약 5.6초 → 4.2초.
      더 줄이기 어려운 이유: 회선이 대역폭 한계(약 12MB/s)라 병렬로 받아도 총량은
      거의 그대로이고(이득은 받는 동안 파싱이 겹치는 데서 온다), 남은 것은 번들
      파싱(약 1.7초)과 동기 XHR인 `the language pack`(6MB, 0.5~0.8초)이다.
      gzip은 오히려 손해다(§2).
- [x] M2.11 ES 모듈(`modules.rs`). rquickjs의 `loader` feature(순수 Rust `relative-path`
      하나 추가)로 QuickJS에 경로 해석기와 로더를 단다. 모듈 이름은 절대 URL이라 모듈
      자신이 import의 기준이 된다. 상대·절대 지정자는 URL로, bare 지정자는
      `<script type=importmap>`의 `imports`로(정확히 맞는 것, 아니면 가장 긴 `/` 접두어)
      풀고, 없으면 Chrome과 같은 "Failed to resolve module specifier" 오류다. 기준이 URL이
      아니면(클래식 스크립트의 `import()`, `Runtime.evaluate`) 문서 URL을 쓴다. 로딩은
      엔진의 `Client`라 쿠키·`Network.*`·미리 받기가 그대로 적용되고, 파서가 찾은 모듈
      `src`와 `<link rel=modulepreload>`는 파싱 직후 미리 받는다. `import.meta.url`을
      준다. 인라인 모듈은 `문서URL#inline-module-N`이라는 고유한 이름으로 선언한다.
      실행 순서: 모듈은 인라인도 `defer`처럼 문서 순서대로, `async`면 async 순서로,
      스크립트가 넣은 모듈은 태스크에서. top-level await와 모듈 안팎의 `import()`가 된다.
      평가가 끝나면 외부 모듈은 `load`, 실패하면 `error`. `nomodule` 스크립트는 이제
      돌지 않는다(모듈을 지원하는 브라우저처럼). 없는 것: import map의 `scopes`·
      `integrity`, JSON·CSS 모듈, MIME 검사. vite.dev(VitePress)가 네이티브와 WASIX에서
      오류 없이 하이드레이션된다. 모듈 실행으로 0.3~1초가 더 걸린다.
- [x] M2.16 `innerText`(`text.rs`). HTML의 rendered text collection을 계산 스타일로 한다.
      `display:none`(UA 시트상 script·style·head 포함)은 빠지고, `visibility:hidden` 글자는
      빠지되 보이는 자식은 남는다. `white-space`대로 공백을 접고 줄 앞뒤 공백을 지운다. 블록은
      앞뒤 한 줄, `<p>`는 빈 줄, `<br>`은 줄바꿈, 표 칸은 탭, 행은 줄바꿈. 이미지 같은 인라인
      객체가 있는 줄은 빈 줄로 보지 않는다. `text-transform`을 적용한다. 렌더링되지 않는
      요소는 규격대로 `textContent`. 참조 사이트 로그인 상자는 Chrome과 글자까지 같고, 웃긴대학
      본문 첫 400자는 빈 줄 3개(Chrome이 `<br>` 등에서 따로 내는 `\n`) 말고는 같다. 이때 UA
      시트에 `pre`의 `white-space: pre`, `textarea`의 `pre-wrap`, `option`의 `display: block`이
      빠져 있던 것도 넣었다(레이아웃 비교 결과는 그대로).
- [x] M2.15 id 전역 이름(named access). `<div id=menu>`가 `window.menu`이고 맨 이름
      `menu`로도 불린다. `<img>`·`<form>`·`<embed>`·`<object>`는 `name`으로도. 브라우저처럼
      창과 `Window.prototype` 사이에 Proxy를 끼우고, 진짜 전역과 스크립트가 대입한 값이
      먼저다. 조회는 Rust의 id·name 색인(`DomState::named_item`, 문서 버전이 바뀔 때만 다시
      만듦)이라 번들의 잦은 `typeof module` 같은 검사가 느려지지 않는다(참조 사이트 로딩
      그대로). QuickJS는 전역 프로토타입 체인의 Proxy를 만나면 `get`의 `undefined`를 그대로
      써서 선언되지 않은 이름이 ReferenceError가 아니게 됐다. 그래서 QuickJS를 패치했다
      (`vendor/patches/0002`): 전역 변수 조회가 이국 객체를 만나면 `has`를 먼저 묻는다.
      원본 소스의 `XXX: should pass throw_ref_error` 자리다. 이때 vendored `build.rs`가
      C 소스 변경을 감시하지 않는 것도 고쳤다(`rerun-if-changed=quickjs`).
- [ ] M2.12 `popstate`. 지금은 `pushState`로 만든 항목으로 뒤로 가면 그 URL을
      새로 읽는다.
- [x] M2.5 마우스. `Input.dispatchMouseEvent`가 히트 테스트로 대상을 찾고
      pointer/mouse over·out·enter·leave·move·down·up, 포커스 이동, `click`과 그
      기본 동작, `dblclick`, `contextmenu`, `wheel`을 브라우저 순서로 낸다. 누른
      요소와 뗀 요소가 다르면 공통 조상이 클릭된다. `DOM.getBoxModel`·
      `getContentQuads`가 같은 레이아웃에서 나와서 agent-browser의 ref 클릭이 된다.
- [x] M2.6 이벤트 루프. 스크립트마다 마이크로태스크 체크포인트를 돌고,
      타이머·애니메이션 프레임·네트워크 완료는 **가상 시계** 위의 작업 큐로 돈다.
      다음 작업이 미래면 시계를 건너뛰므로 5초 타이머가 5초를 쓰지 않는다.
      `Date.now()`와 `performance.now()`도 같은 시계를 본다. 로드 뒤 5초,
      `Runtime.evaluate` 뒤 1초분의 가상 시간을 흘린다.

### 4.4 M3 네트워크와 저장소 (P1)

- [x] M3.1 `Network.*` 이벤트. Chrome과 agent-browser 사이를 캡처해서 맞췄다:
      클라이언트가 보내는 것은 `Network.enable`(버퍼 크기 인자)과
      `Network.getResponseBody`뿐이고, 받는 것은 요청마다 `requestWillBeSent` →
      `responseReceived` → `dataReceived` → `loadingFinished`다. 리다이렉트는 같은
      ID로 `requestWillBeSent`를 다시 내고 `redirectResponse`를 붙이며, 실패는
      `loadingFailed`다. 종류는 Document·Stylesheet·Script·Fetch·XHR. `Client`가
      어느 스레드에서든 `network.rs`에 기록하고, 엔진이 로드의 생명주기 이벤트 앞과
      모든 명령의 끝에서 가리기 출구로 내보낸다. `network requests`, `network
      request <id>`(본문 포함), HAR 기록이 Chrome과 같은 모양으로 나온다.
      Chrome과 다른 점: `*ExtraInfo` 이벤트는 내지 않고 `hasExtraInfo: false`로
      밝힌다(원시 헤더와 쿠키), 응답 헤더 이름이 소문자다(HTTP 파서가 정규화),
      `Referer`를 보내지 않는다, 타이밍 세부와 `postDataEntries`가 없다.
      메모리: 본문을 클라이언트가 준 상한(agent-browser는 100MB)까지 보관하므로
      참조 사이트를 세 번 열면 RSS가 397MB에서 478MB가 된다.
      이 작업에서 고친 것: 가리기가 JSON 이스케이프·폼 인코딩된 비밀도 찾는다(로그인
      POST 본문이 그 모양이다). `fetch`/XHR의 기본 `Accept`가 `*/*`다.
- [x] M3.2 쿠키를 구현한다. **로그인 흐름에 필수다.** `cookies.rs`의 메모리 전용
      쿠키 저장소를 모든 타깃이 공유한다. RFC 6265의 도메인·경로 매칭, 만료,
      `Secure`·`HttpOnly`를 지킨다. `fetch::Client`가 리다이렉트 응답의 쿠키까지
      저장하고, POST 뒤 302·303은 GET으로 바꾼다. CDP의 쿠키 메서드와
      `document.cookie`가 같은 저장소를 쓴다. 공개 접미사 목록은 없다.
- [x] M3.7 콘솔과 잡히지 않은 예외. Chrome과 agent-browser 사이를 캡처해서 맞췄다:
      `console`은 `Runtime.consoleAPICalled`의 `type`과 `args`(원격 객체, 객체는 `preview`로
      `{a: 1}`처럼 보인다)로, `errors`는 `Runtime.exceptionThrown`(`Uncaught`,
      `Uncaught (in promise)`)으로 만든다. 프렐류드가 `console`을 감싸 log·info·debug·
      warn(`warning`)·error·trace·dir·dirxml·table·assert·count·time·group·clear를 큐에
      넣고(최대 1000개), 호스트 로그에도 한 줄 남긴다(`probe_a_url`용). `runScript`가 오류
      문자열과 함께 던진 값을 돌려줘서, 스크립트·리스너·타이머의 잡히지 않은 예외가 모두
      `reportError` 한 곳에서 `exceptionThrown`이 된다(QuickJS 스택의 첫 프레임에서 줄·열,
      0부터). 스크립트 오류가 이제 window `error` 이벤트도 쏜다(전에는 안 쐈다). 처리되지
      않은 Promise 거부는 rquickjs의 `set_host_promise_rejection_tracker`로 모았다가 태스크
      사이에 `unhandledrejection`을 쏘고, 막지 않았으면 보고한다. 나중에 처리기가 붙으면
      빠진다. 엔진은 명령·이동이 끝날 때와 문서를 바꾸기 직전에 큐를 비우고
      `Runtime.enable` 상태일 때만 시각과 컨텍스트를 붙여 내보낸다(가리기 출구를 지난다).
      오류 설명과 미리보기의 스택에서 프렐류드 프레임(`eval_script`)은 뺀다.
      `Runtime.discardConsoleEntries`도 받는다. 참조 사이트 로그인 화면은 콘솔 29줄,
      잡히지 않은 오류 0개다. 없는 것: `stackTrace` 필드, `Log.entryAdded`(네트워크 오류
      같은 브라우저 자체 메시지), `exceptionRevoked`.
- [x] M3.6 실시간 보기(`screencast.rs`, `scripts/watch.sh`). agent-browser의 대시보드
      (`agent-browser dashboard`, `http://localhost:4848`)와 `stream`은 CDP
      `Page.startScreencast {format: jpeg, quality: 80, maxWidth, maxHeight}`로 프레임을 받고
      `Page.screencastFrameAck`로 응답하며, 사람의 입력을 `Input.dispatchMouseEvent`와
      `Input.dispatchKeyEvent`(글자 키는 `keyDown`에 `text`)로 보낸다(Chrome과 캡처해서
      확인). 합성기가 없으므로 엔진은 명령과 이동이 끝날 때마다 화면을 바꿀 수 있는 것
      (문서 버전, 이미지 세대, 뷰포트, URL, 포커스)이 바뀌었는지 보고, 바뀌었고 직전
      프레임에 응답이 왔을 때만 그려서 보낸다. 읽기만 하는 명령에는 비용이 없다.
      포커스된 입력란에는 커서를 그린다(포커스 테두리는 `:focus` 규칙이 캐스케이드에
      없어서 그리지 않는다). `Page.captureScreenshot`도 JPEG를 낸다(WebP는 오류).
      `--cdp`로 연결한 세션도 대시보드에 나온다. 네이티브와 WASIX에서 스트림으로
      클릭·입력·Backspace가 Chrome과 같은 결과를 낸다. 이 작업에서 고친 것: 출구의 문자열
      가리기가 스크린샷의 base64까지 훑어서, 짧은 비밀이 우연히 들어 있으면 이미지가
      깨질 수 있었다. 이진 데이터 필드는 인코딩 전에 가렸으므로 문자열 가리기에서 뺀다.
- [x] M3.5 `localStorage`·`sessionStorage`는 출처별로 나뉘어 엔진 메모리(`DomState`)에
      있어 페이지를 옮겨도 남는다. 디스크에는 쓰지 않는다. 다만 탭(타깃)마다 따로라
      `localStorage`가 탭 사이에 공유되지 않는다. 쿠키는 공유된다.
- [ ] M3.3 요청 가로채기를 구현한다. `agent-browser network route`. CDP로는
      `Fetch.enable` 뒤 `Fetch.requestPaused`를 받은 클라이언트가
      `Fetch.fulfillRequest`/`continueRequest`/`failRequest`로 답한다. 지금 엔진은
      명령 하나를 처리하는 동안 다음 명령을 받지 않으므로, 페이지 로드 중에 멈춘
      요청이 그 답을 기다리면 교착한다. 로드를 디스패치 밖으로 빼거나, 규칙을
      엔진에 미리 넘기는 방식이 필요하다.
- [x] M3.4 `Page.captureScreenshot`. M1.5 참조.

### 4.5 M4 자격증명 (P1, 목표의 핵심)

- [x] M4.1 자격증명은 stdin으로만 받는다(`--secret 이름@출처`, 값은 한 줄씩).
      인자·환경 변수·파일로는 받지 않고, stdin이 터미널이면 거부한다(WASIX에서는
      `wasix-run.sh`가 호스트에서 검사). 클라이언트는 `{{secret:이름}}`을 `fill`하고,
      엔진이 `Input.insertText` 안에서 그 출처의 페이지에만 값을 넣는다. 엔진이
      내보내는 모든 CDP 결과·오류·이벤트와 탭 제목·URL에서 값을 자리표시자로
      되돌린다. 값은 해제될 때 덮어쓴다. [docs/credentials.md](credentials.md).
- [x] M4.2 agent-browser의 auth vault는 쓰지 않는다. 자리표시자로 대신한다.
- [x] M4.3 유출 경로 점검. CDP 결과·오류·이벤트, 접근성 트리, 추적을 켠 엔진
      stderr, 디스크는 확인했다(`verify-login.sh`, 네이티브·WASIX). `Network.*`와
      스크린샷은 M3.1·M1.5에서 막았다. 콘솔은 아직 내보내지 않고, 의존성의 panic
      메시지는 통제하지 못한다. 악의적인 클라이언트는 한 글자씩 빼낼 수 있다(문서에 명시).

## 5. 미결 사항

- [x] **QuickJS를 패치할지 (M2.10).** 했다. rquickjs-sys를 빌드에 쓰는 파일만
      남겨 `vendor/rquickjs-sys`(4.4MB)에 두고 `[patch.crates-io]`로 바꿔 끼웠다.
      패치는 `vendor/patches/`에 diff로도 둔다. `Context::full` 문제(sysroot)도
      이제 같은 자리에서 고칠 수 있다.

- [ ] **`Context::full`을 살릴지.** `wasixcc`는 이미 설치되어 있고, QuickJS도 이제
      `vendor/`에서 직접 패치할 수 있다. 하지만 `performance`는 프렐류드가 가상 시계로
      제공하므로 급하지 않다.
- [ ] **클라이언트를 무엇으로 할지.** 세 후보다.
      `agent-browser`는 Rust CLI라 `cargo wasix`로 WASIX에 올릴 수 있고,
      `inno-creed`를 올린 경로와 같다. Playwright는 Node 기반이고 페이지 JS에
      훨씬 많이 의존하므로 M2가 훨씬 두꺼워져야 한다. 자체 클라이언트를 만들면
      표면을 우리가 정할 수 있지만 기존 도구 생태계를 잃는다.
- [ ] **백로그 말고 다른 문서도 한국어로 할지.** 현재 README와 `docs/*.md`는
      영어입니다.

## 6. 알려진 함정

되짚지 않도록 적어둡니다.

- **소켓 거부가 `EOPNOTSUPP`로 온다.** WASIX가 소켓을 못 여는 것처럼 읽히지만
  런타임 정책이며, Wasmer에서는 `--net`으로 풀린다.
- **`agent-browser --cdp`는 포트나 완전한 URL만 받는다.** `host:port`는
  거부한다.
- **하니스의 `proc.stdout`은 `ReadableStream`이 아니다.** `ReadableBytes`이고
  `lines()` 비동기 제너레이터를 쓴다. `pipeTo`는 실패한다.
- **`@wasmer/sdk`는 파일 배치를 그대로 유지해야 한다.** 자기 워커와
  wasm-bindgen 모듈을 `import.meta.url` 기준으로 찾는다.
- **`whb`의 accept 루프가 `os error 73`을 찍는다.** 클라이언트가 끊길 때
  나오는 잡음이고 동작에는 영향이 없다. 다만 로그에는 남으므로 나중에
  조용히 하거나 이유를 밝히는 편이 좋다.
- **`@wasmer/sdk`는 JSPI를 요구한다.** `SharedArrayBuffer`만으로는 부족하다.
  Chrome이나 Edge가 필요하다.
- **`sha1` 크레이트를 쓰면 WASIX 빌드가 깨진다.** `cpufeatures`를 거쳐
  `libc`를 끌어오는데 WASIX 레지스트리에 맞는 버전이 없다. 지금은 리포 안에
  직접 구현이 있다.
- **`cargo-wasix`가 `.cargo/config.toml`을 쓴다.** crates.io를 WASIX 오버레이
  레지스트리로 바꿔치기하므로 gitignore에 두었다.
- **`Cargo.lock` 하나로 두 레지스트리를 다 맞출 수 없다.** WASIX 레지스트리에는
  `libc`·`cc`·`ring`·`getrandom`의 `+wasix` 빌드가 있고 crates.io에는 그것이
  없다. 그래서 서로의 락파일에서 해석에 실패한다. 커밋하는 락파일은 crates.io용이다.
  `scripts/wasix-build.sh`가 빌드 동안만 `WASIX_FORKS`의 패키지를 다시 고정하고,
  끝나면 락파일을 되돌리고 `.cargo/config.toml`을 지운다. 그 파일이 남아 있으면
  네이티브 빌드가 깨지기 때문이다. 새 의존성이 "candidate versions found which
  didn't match: X+wasix.1"로 실패하면 X를 `WASIX_FORKS`에 추가한다.
- **엔진에서 `rquickjs`의 `full-wasi`를 쓰면 WASIX 빌드가 깨진다.**
  `full-wasi`가 `rquickjs-macro`를 끌어오는데, WASIX 툴체인의 rustc가
  cargo가 만든 그 proc-macro를 읽지 못한다 (E0463 "can't find crate").
  단순한 proc-macro는 읽으므로 원인은 아직 모른다. 엔진은 매크로를 쓰지 않으므로
  `bindgen`만 켠다.
- **rquickjs의 `eval`은 기본이 strict 모드다.** 페이지 스크립트는
  `EvalOptions`로 strict를 끄고 평가한다.
- **`agent-browser`는 0.38.2로 검증했다.** 0.16.3에서는 모든 명령이
  "No page found"로 실패한다. 다른 버전을 쓸 때는
  `npm install --prefix /tmp/ab agent-browser@0.38.2`로 따로 받아 `PATH` 앞에 둔다.
- **Wasmer는 최신이 필요하다.** 지금 WASIX 툴체인은 wasm 예외 제안을 쓴다.
  Wasmer 4.2.6은 "exceptions proposal not enabled"로 모듈을 거부하고,
  7.5.0은 동작한다.
- **Wasmer의 소켓은 주소를 재사용하지 않는다.** 앞선 실행이 포트를 놓고
  약 30초(macOS의 TIME_WAIT) 동안은 bind가 `AddrInUse`로 실패한다. 네이티브
  빌드는 바로 bind한다. `verify-m0.sh`는 WASIX 모드에서 이 오류가 나면
  다시 시도한다.
- **JS 컨텍스트는 문서마다 새로 만든다.** 한때 하나를 계속 썼더니 이전 페이지의
  전역이 남았고, 같은 페이지를 다시 열면 최상위 `const`가 재선언 오류를 냈다.
  `replace_document`가 `ScriptHost::reset`을 부른다. 런타임은 그대로 쓴다.
- **DOM 프렐류드는 `crates/engine/src/prelude/*.js`다.** 파일 이름 순서대로
  이어 붙여 함수 하나로 감싼다(`bindings.rs`의 `PRELUDE`). 한 파일에 쓰기에는
  너무 커서 나눴다. 파일끼리 스코프를 공유하고, 전역에는 `80-global.js`가 내보낸
  것만 보인다.
- **프렐류드에서 getter가 있는 객체를 `{ ...obj }`로 펼치면 안 된다.** 펼치는 순간
  getter가 실행된다. 인터페이스 본문은 `defineElement(name, Parent, a, b)`처럼
  나눠 넘기면 descriptor째 복사된다. `Object.assign`도 같은 이유로 getter에 쓰지 않는다.
- **QuickJS의 `DOMException`은 `Error`를 상속하지 않는다.** 그대로 던지면
  rquickjs가 "Exception generated by QuickJS"만 보여 준다. 프렐류드가 프로토타입을
  `Error.prototype`으로 이어 붙인다.
- **`WHB_TRACE_CDP`는 입력한 글자를 가린다.** 처음에는 `Input.insertText`를 그대로
  찍어서 참조 사이트 시험 때 가짜 비밀번호가 stderr에 남았다. 이제 입력 텍스트와 키
  이벤트의 글자는 길이만 남는다. 다른 메서드는 보낸 그대로 찍으므로, 표현식에 비밀을
  넣는 클라이언트는 거기서 드러난다.
- **taffy 0.14는 잎 노드의 배치 전체를 측정 함수에 맡긴다.** 측정 함수는
  `taffy::compute_leaf_layout`에 내용 크기만 넘겨야 한다. 그렇지 않으면 자식 없는
  `div`의 `height: 50px`가 무시되고 0×0이 된다.
- **Chrome과 레이아웃을 비교하려면** `scripts/compare-layout.sh URL`. Playwright의
  headless shell을 따로 띄우므로 사용자의 Chrome 프로필을 건드리지 않는다. SPA는
  `WAIT_FOR=선택자`를 주고, 애니메이션이 끝나도록 `SETTLE_MS`(기본 2.5초)를 기다린다.
  이걸 안 기다리면 참조 사이트 로그인 상자가 들어오는 도중을 재서 46px 다르게 나온다.
- **대시보드는 agent-browser가 아는 뷰포트로 클릭 좌표를 환산한다.** 화면 속 클릭
  위치 × (agent-browser의 뷰포트 너비 ÷ 그려진 너비)다. agent-browser는 기본값
  1280×720을 알고 있지만 `--cdp`로 붙으면 엔진에 알리지 않아서, 엔진이 800×600이면
  모든 클릭이 1.6배 오른쪽·아래로 떨어진다. `watch.sh`가 `set viewport`로 맞춘다.
- **대시보드는 하나뿐이다.** `dashboard start --port`는 이미 다른 포트에서 돌고 있으면
  실패한다. `watch.sh`는 돌고 있는 것을 쓰고, 자기가 띄운 것만 끈다. 한때 무조건 껐다가
  사용자가 보던 대시보드를 꺼 버렸다.
- **백그라운드로 띄운 스크립트는 SIGINT를 받지 않는다.** 비대화형 셸의 백그라운드
  작업은 SIGINT를 무시한다. `watch.sh`의 정리 경로를 시험하려면 TERM을 보낸다.
  터미널에서 Ctrl-C는 포그라운드 그룹에 가므로 그대로 된다.
- **새로 만든 파일이 읽기 도구에서 안 보일 때가 있다.** 2026-10-10에 `/tmp`의 새 PNG가
  bash에서는 있는데 읽기 도구는 ENOENT를 냈다. 그때는 PIL로 숫자를 뽑아 확인했다.
- **테스트의 `evaluate` 도우미는 태스크를 돌리지 않는다.** 엔진의 원시 평가라서
  `scroll` 이벤트처럼 태스크로 미룬 일은 일어나지 않는다. 클라이언트가 하는 대로 하려면
  `dispatch("Runtime.evaluate")`를 쓴다.
- **agent-browser의 클릭 가능한 `generic` 이름은 `textContent`다.** 참조 사이트 스냅샷
  첫 줄에 숨은 화면의 글자가 섞이는 것은 Chrome에서도 글자까지 같다. 엔진 문제가
  아니다(한때 백로그에 버그로 적었다가 정정).
- **접근성 트리는 `disabled`를 속성으로 낸다.** Chrome처럼 `{name: disabled, value:
  {type: boolean, value: true}}`. 참조 사이트 첫 입력란 칸이 `[disabled]`로 나와야 Chrome과
  같다. 규칙은 프렐류드의 `isDisabled`와 같다(fieldset, 첫 legend 예외, optgroup, aria).
- **vendored C를 고치면 다시 컴파일되는지 확인한다.** 0002 이전에는 `build.rs`가 C
  소스를 감시하지 않아 고쳐도 옛 객체 파일이 쓰였다.
- **배경은 축약 속성과 longhand를 함께 읽는다.** 인라인 `background-image`만 있고
  위치·반복은 시트의 `background` 축약 속성에 있는 경우가 흔하다(참조 사이트). 한쪽만
  읽으면 위치가 가운데로, 반복이 켜진 채로 그려진다.
- **레이아웃은 이미지를 기다리지 않는다.** 스크립트 도중의 `getBoundingClientRect`가
  다운로드를 기다리면 로드가 느려진다. 기다리는 곳은 이미지의 `load` 태스크와 스크린샷뿐이다.
- **글꼴을 바꾸면 레이아웃이 바뀐다.** 글자 폭이 `font.rs`에서 나오기 때문이다.
  `scripts/make-font.sh`로 다시 만든 뒤에는 `compare-layout.sh`로 확인한다.
- **Wasmer에서는 게스트의 stdin이 늘 터미널로 보인다.** 파이프로 넣어도 그렇다.
  그래서 브라우저는 WASI 타깃에서 터미널 검사를 하지 않고, `wasix-run.sh`가 호스트에서
  한다. 값 자체는 파이프로 제대로 전달된다. 또 Wasmer는 호스트의 환경 변수를 넘기지
  않으므로 `WHB_TRACE_CDP` 같은 것은 `wasmer run --env`로 준다.
- **미리 받는 스레드는 `send_direct`로 보낸다.** `send`는 미리 받은 응답부터
  찾으므로, 그 스레드가 `send`를 쓰면 자기 자신의 결과를 기다리며 멈춘다.
- **미리 받은 응답은 헤더 없는 `GET`(`Accept`만 허용)에만 준다.** 인증 헤더 같은
  것을 단 요청은 따로 보낸다. 문서가 바뀌면 쓰지 않은 것은 버린다.
- **병렬 다운로드 테스트는 `test_server::serve_together`로 한다.** 지정한 경로가
  모두 요청될 때까지 답하지 않으므로, 하나씩 받는 코드는 3초 뒤 503을 받고 실패한다.
- **agent-browser의 `type`은 한 글자씩 `Input.insertText`를 보낸다.** 그래서
  자리표시자는 `fill`로만 동작한다.
- **클라이언트가 무엇을 보내는지는 `WHB_TRACE_CDP=1`로 본다.** 모든 CDP 요청과
  응답이 잘려서 stderr로 나온다. 예를 들어 agent-browser의 `fill`은
  `DOM.resolveNode` → `callFunctionOn(focus)` → `callFunctionOn(값 지우기)` →
  `Input.insertText`이고, CSS 선택자는 `Runtime.evaluate(querySelector)`의 핸들로 받는다.
- **사이트가 왜 안 뜨는지는 `probe_a_url`로 본다.**
  `WHB_PROBE_URL=https://... cargo test -p engine probe_a_url -- --ignored --nocapture`.
  콘솔과 스크립트 오류를 출력한다. CDP로 콘솔을 내보내는 것(`Runtime.consoleAPICalled`)은
  아직 없다.
- **테스트에서 `engine.doc()`을 한 문장에 두 번 쓰면 교착한다.** 첫 번째
  가드가 문장 끝까지 살아 있기 때문이다. 문서 ID를 먼저 변수로 받는다.
- **QuickJS는 `vendor/rquickjs-sys`에서 온다.** rquickjs를 올릴 때는 새
  rquickjs-sys를 같은 방식으로 복사하고 `vendor/patches/`를 다시 적용한다.
  경로 의존성이 되면서 빌드 때 "using bundled bindings" 경고가 보이는데, 원래도
  나던 것이 이제 숨겨지지 않을 뿐이다.
- **`rquickjs`의 `intrinsic::Performance`는 존재하지 않는 심볼을 가리킨다.**
  실제 이름은 `JS_AddPerformance`다. WASIX와 무관한 rquickjs 쪽 문제이므로
  그 타입은 쓰지 않는다.

## 7. 하지 않기로 한 것

- **렌더링 파이프라인과 합성기.** 자동화에는 필요 없다. WASI에 GPU 표면
  인터페이스가 없다는 문제도 함께 사라진다.
- **호스트 페이지에서 샌드박스 안으로 들어오는 경로.** 필요 없고, 되더라도
  HTTP 전용이라 CDP가 지나가지 못한다.
- **호스트 OS에서 샌드박스 안으로 들어오는 경로.** 브라우저 샌드박스에는
  실제 소켓이 없다. Wasmer CLI에서는 되지만 쓰지 않는다.
