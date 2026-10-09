# Backlog

작업 기록입니다. 이미 측정으로 확정한 사실과, 아직 남은 일을 나눠 적었습니다.
문서가 영어로 된 다른 파일들과 섞이지만, 이 문서는 작업용이라 한국어로 씁니다.

마지막 갱신: BL-010 검증 완료. M2의 엔진 연결부가 네이티브와 WASIX 양쪽에서 동작한다.

## 1. 현재 위치

| 단계 | 내용 | 상태 |
|---|---|---|
| M0 | CDP 프로토콜 경로와 엔진 경계 확정 | 완료 |
| — | WASIX 네이티브 실행 (loopback, 검증 16건) | 완료 |
| — | 브라우저 샌드박스 loopback (E0·E1·E2) | 완료 |
| — | 브라우저 샌드박스 외부 통신 (E4·E5) | 완료 |
| — | QuickJS 실행 가능성 확인 | 완료 |
| M1 | 실제 엔진: HTML 파싱, DOM, 접근성 트리, 근사 레이아웃 | 진행 중 (레이아웃 제외 완료) |
| M2 | JavaScript 엔진과 DOM 바인딩 | 진행 중 (M2.1–M2.3 완료) |
| M3 | 네트워크 이벤트, 쿠키·스토리지, 스크린샷 | 착수 전 |
| M4 | 자격증명을 메모리로만 다루기 | 착수 전 |

`--engine html`로 실제 페이지를 가져와 파싱하고, 그 문서에서 접근성 트리를
만들어 냅니다. `agent-browser`로 열고 스냅샷을 뜨면 진짜 페이지가 나옵니다.

페이지의 인라인 스크립트가 QuickJS로 실행되고, 그 결과가 접근성 트리에
반영됩니다. `Runtime.evaluate`도 실제 JS입니다. 외부 스크립트(`src`)와
모듈은 아직 실행하지 않습니다.

아직 없는 것은 레이아웃입니다. **흉내 내지 않고 이유를 말합니다.**
스크린샷과 입력은 "레이아웃이 없다"는 오류를 돌려줍니다.

## 1.5 BL-010 검증 결과

검증한 것:

- 워크스페이스 전체 `cargo test`: 엔진 92개, `cdp-wire` 3개 통과
- `./scripts/verify-m0.sh`: 16개 통과
- `./scripts/verify-m1.sh`: 26개 통과. 픽스처에 인라인 스크립트를 넣고
  스크립트가 만든 제목, `eval` 결과, 스크립트가 남긴 전역을 검사 4개로 확인한다
- `MODE=wasix ./scripts/verify-m0.sh`: 16개 통과. 연달아 실행해도 통과한다
- WASIX 모듈 크기는 1.6MB다 (QuickJS 이전에는 272KB)
- WASIX에서 `--engine html`로 픽스처를 열어 인라인 스크립트, `eval`, DOM
  읽기, `Date`가 동작하는 것을 손으로 확인했다

검증하면서 고친 것:

- **`textContent`에 setter가 없었다.** rquickjs는 기본이 strict 모드라
  대입하면 "no setter for property" 예외가 났다. setter를 붙이고,
  `id`·`className`·`title`·`href` 같은 속성 반영 프로퍼티에도 setter를 붙였다
- **평가를 sloppy 모드로 바꿨다.** 브라우저의 일반 `<script>`와
  `Runtime.evaluate`는 sloppy다. strict면 선언 없는 대입 같은 흔한 페이지
  코드가 예외를 던진다
- **DOM 객체가 클래스 이름을 `Object`로 냈다.** 래퍼에 `Symbol.toStringTag`를
  붙여 `HTMLParagraphElement` 같은 인터페이스 이름을 낸다.
  `DOM.resolveNode`와 같은 `class_for` 표를 쓴다
- **정수가 `42.0`으로 나갔다.** Chrome처럼 `42`로 낸다
- **테스트 6개가 스스로 교착했다.** `engine.doc().find_id(engine.doc()...)`가
  한 문장에서 같은 뮤텍스를 두 번 잡는다. 엔진 코드의 문제가 아니라 테스트의
  문제였다
- **`full-wasi` feature를 뺐다.** §6의 `rquickjs-macro` 항목 참조

남은 것: `Runtime.callFunctionOn`은 아직 실제 JS가 아니다. 함수 선언 문자열에
`textContent`나 `value`가 들어 있는지만 본다. M2.4에서 원격 객체 핸들과 함께
바꾼다.

### 설계상 알아둘 것

`rquickjs::Runtime`은 `Send`가 아닙니다. `Send`가 열리는 `parallel` feature가
tokio의 멀티스레드 런타임을 끌어오기 때문입니다. 그래서 JS 런타임을
**전용 스레드에 가두고 채널로 통신**합니다(`ScriptHost`). 엔진은 `Sender`만
들고 있으므로 `Engine: Send`가 유지됩니다.

DOM은 가두지 않았습니다. `Arc<Mutex<Document>>`로 엔진과 공유하므로 페이지
스크립트가 만든 변경이 접근성 트리에 즉시 반영됩니다. 다만 **JS를 호출할 때
문서 잠금을 잡고 있으면 안 됩니다.** 스크립트 스레드가 같은 뮤텍스를 잡으므로
교착합니다. `load()` 류가 잠금 안에서 값을 모아 두었다가 잠금을 놓은 뒤
필드에 대입하는 이유가 이것입니다.

또 하나: 매크로로 원시 접근자를 생성하려 하면 **매크로 위생 때문에 실패합니다.**
호출부의 `doc`은 함수 인자(참조)로 해석되어 클로저가 `'static`이 되지 못합니다.
그래서 `put_reads`/`put_content`/`put_mutations`는 명시적으로 써 두었습니다.

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
| QuickJS를 넣은 WASIX 모듈은 1.6MB다. TLS를 넣으면 2.3MB다 | 이 문서의 §1.5, [docs/wasix.md](wasix.md) |
| WASIX에서 rustls + ring이 TLS 1.3으로 동작한다. 참조 사이트 인증서는 공개 루트로 검증된다 | [docs/wasix.md](wasix.md) |
| example.com은 이제 본문이 문단 하나와 `<script src=/s.js>`뿐이다. 외부 스크립트 없이는 제목과 링크가 없다 | 2026-10-09 측정 |
| 참조 사이트 로그인 화면을 800×600에서 Chrome(Playwright headless shell)과 비교하면 입력란·`.login-form`·`#wrap`·저작권 문구의 위치와 크기가 같거나 1px 차이다. 체크박스만 x가 8px 다르다. 아이디 입력란 클릭은 포커스를, "체크박스" 글자 클릭은 체크를 두 엔진에서 똑같이 바꾸고, 숨은 체크박스를 ref로 누르면 두 엔진 모두 빗나간다(`div.login-background`가 맞는다) | 2026-10-09 |
| WASIX 모듈은 레이아웃까지 넣어 3.3MB다 | 같은 날 |
| 캐스케이드를 넣은 뒤 참조 사이트 "다음" 버튼의 계산 스타일은 `display: block`, `cursor: pointer`, `width: 100%`, `height: 54px`, 배경 `#00aaff`(`a CSS variable`에서), 13.33px이다. 전체 스냅샷은 23줄이다 | 2026-10-09 |
| 참조 사이트의 숨은 화면(추가 인증 선택, OTP, 실패 대화상자)은 CSS 클래스가 아니라 인라인 `style="display: none"`으로 숨는다. 인라인 스타일만 반영해도 로그인 화면의 인터랙티브 스냅샷이 25개 넘던 것에서 4개(첫 입력란, 아이디, 다음, 체크박스)로 준다 | 2026-10-09 측정 (M1.6) |
| 참조 사이트에 가짜 계정으로 로그인하면 실패가 화면에 나온다. Enter 한 번에 아이디 단계(`POST /api/login-step`, a result code), 버튼이 "로그인"으로 바뀐 뒤 Enter 한 번 더에 인증(a result code "실패 문구")이고, 그 문구가 폼 아래 문단과 확인 대화상자로 스냅샷에 나온다. 클릭 없이 Enter만으로 된다 | 2026-10-09, 사용자 승인 아래 가짜 계정 `nonexistent-test-account`로 |
| 참조 사이트의 React 입력란에 agent-browser `fill`이 동작한다. 아이디는 스냅샷에 나오고 비밀번호는 나오지 않는다 | 2026-10-09 agent-browser |
| 참조 사이트의 로그인 화면이 WASIX에서 그려진다. 첫 입력란·아이디·비밀번호 입력란과 "다음" 버튼이 스냅샷에 나온다. CSS로 숨긴 추가 인증 화면들도 함께 나온다(캐스케이드 없음) | 2026-10-09 agent-browser |
| QuickJS 호출은 V8보다 네이티브 스택을 훨씬 많이 쓴다. 기본 한도 1MB로는 React 렌더링이 넘친다. 스크립트 스레드는 32MB 스택에 한도 24MB로 돈다 | 같은 측정 |
| 참조 사이트는 React 번들 3개와 the grid library·jQuery·lottie를 싣는다. jQuery는 초기화 때 `createHTMLDocument`를, lottie는 캔버스 2D 컨텍스트를 요구하고, the grid library는 `Function.caller`를 요구한다 | 2026-10-09 `probe_a_url` |
| agent-browser의 `back`·`forward`는 CDP 메서드가 아니라 `Runtime.evaluate("history.back()")`이다 | [docs/capture/required-expressions.txt](capture/required-expressions.txt) |
| `Context::full`이 죽는 원인은 sysroot 불일치 하나다. `CLOCK_MONOTONIC`이 포인터 인코딩(1058440)으로 컴파일되고 WASIX는 정수 1을 기대한다 | 같은 문서 |
| 참조 사이트 사이트는 SPA 셸만 돌려준다. HTML만 받아서는 아무것도 얻을 수 없다 | 이 문서의 §3 |

## 3. 방금 확인한 것 (E4·E5)

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
      처리한다. HTTPS는 TLS를 게스트로 가져와야 하므로 아직 없고, 그 사실을
      메시지로 밝힌다. `std::net`으로 평문 HTTP를 열면
      네이티브와 WASIX 양쪽에서 동작하고, 브라우저 샌드박스에서는 E4·E5로
      확인한 대로 WISP가 자동으로 처리한다. **WISP를 인지하는 코드는 필요 없다.**
      HTTPS는 TLS를 게스트로 가져와야 하므로 별도 조각으로 둔다.
- [x] M1.11 HTTPS. rustls + ring + `webpki-roots`로 TLS를 게스트 안에서
      처리한다. 사설 CA는 `WHB_EXTRA_CA_CERTS`로 추가한다. WASIX에서
      example.com과 참조 사이트가 TLS 1.3으로 열린다. 근거는
      [docs/wasix.md](wasix.md)의 "TLS inside the guest".
- [ ] M1.9 UTF-8이 아닌 문자셋을 디코딩한다. 지금은 헤더가 UTF-8이 아니면
      손실 변환하고 그 사실을 `last_error`에 남긴다.
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
      남은 근사: 기준선 정렬 없음, 스크롤 없음(`scrollIntoView`는 아무것도 안 함),
      회전·확대 무시, 쌓임 맥락은 한 단계로 평평하게. 목표는 픽셀이 아니라
      `getBoundingClientRect`가 0이 아닌 사각형을 돌려주고 가시성을 판정하는
      것이다. 최소 CSS 캐스케이드(태그·클래스·인라인)가 함께 필요하다.
- [ ] M1.6 측정은 참조 사이트 로그인 화면으로 했다(아래 §2 표). 다른 화면은 아직이다.
- [ ] M1.5 `engine-fake`의 PNG 렌더러를 실제 레이아웃에 연결한다. 현재는
      저장 방식 deflate라 결과가 큽니다. `miniz_oxide` 도입을 검토한다.
- [ ] M1.6 같은 캡처 방식으로 CDP 표면을 다시 측정한다. 실제 사이트를
      대상으로 어떤 메서드가 더 필요해지는지 확인한다.
- [x] M1.7 `browser` 바이너리에 `--engine fake|html` 플래그를 넣었다.
      가짜 엔진이 기본값이라 `verify-m0.sh`는 그대로 통과한다.
- [x] M1.8 `scripts/verify-m1.sh`. 픽스처를 HTTP로 서빙하고, 가져와 파싱한 뒤,
      접근성 트리의 역할·이름·상태를 22개 검사로 판정한다. 스크린샷이 조용히
      빈 이미지를 내지 않고 이유를 말하는지도 검사한다.

### 4.3 M2 JavaScript (P0, 큼)

- [x] M2.1 QuickJS를 엔진 모듈에 통합한다. `Context::custom` 경로를 쓴다.
      `wasixcc`를 설치하면 `Context::full`로 단순해진다 (§5 참조).
- [x] M2.2 DOM 바인딩을 만든다. `document`, `window`, `querySelector`,
      `getComputedStyle`, `getBoundingClientRect`, `focus`, `scrollIntoView`,
      `elementFromPoint`가 필요하다. 앞의 다섯 개는 `agent-browser`가 실제로
      호출하는 것을 캡처로 확인했다. 레이아웃이 없으므로 사각형은 0이고
      `elementFromPoint`는 `null`이다.
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
- [ ] M2.13 참조 사이트를 여는 데 3.5초, 스타일시트 6개(2.8MB)를 받고 파싱하게 된
      뒤로는 5.6초 걸린다. 시트를 병렬로 받거나 스크립트와 겹쳐 받으면 줄어든다.
      이전 기록: 3.5초. 12초 중 9초는 요청 69개가 매번
      새로 맺는 TCP·TLS였고, keep-alive 연결 풀로 줄였다. 남은 시간은 대부분
      7MB 남짓한 번들의 파싱이다(the largest bundle 0.75초, main 0.54초, the grid library 0.43초).
      청크를 병렬로 받으면 조금 더 줄 수 있다.
- [ ] M2.11 ES 모듈(`type=module`)과 `import()`.
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

- [ ] M3.1 `Network.*` 이벤트를 낸다. `agent-browser`가 HAR과 `route`에 쓴다.
- [x] M3.2 쿠키를 구현한다. **로그인 흐름에 필수다.** `cookies.rs`의 메모리 전용
      쿠키 저장소를 모든 타깃이 공유한다. RFC 6265의 도메인·경로 매칭, 만료,
      `Secure`·`HttpOnly`를 지킨다. `fetch::Client`가 리다이렉트 응답의 쿠키까지
      저장하고, POST 뒤 302·303은 GET으로 바꾼다. CDP의 쿠키 메서드와
      `document.cookie`가 같은 저장소를 쓴다. 공개 접미사 목록은 없다.
- [ ] M3.5 `localStorage`를 출처별로 나눈다. 지금은 문서마다 새로 만들어지는
      JS 맵이라 페이지를 옮기면 사라진다.
- [ ] M3.3 요청 가로채기를 구현한다. `agent-browser network route`.
- [ ] M3.4 `Page.captureScreenshot`을 실제 렌더 결과에 연결한다.

### 4.5 M4 자격증명 (P1, 목표의 핵심)

- [x] M4.1 자격증명은 stdin으로만 받는다(`--secret 이름@출처`, 값은 한 줄씩).
      인자·환경 변수·파일로는 받지 않고, stdin이 터미널이면 거부한다(WASIX에서는
      `wasix-run.sh`가 호스트에서 검사). 클라이언트는 `{{secret:이름}}`을 `fill`하고,
      엔진이 `Input.insertText` 안에서 그 출처의 페이지에만 값을 넣는다. 엔진이
      내보내는 모든 CDP 결과·오류·이벤트와 탭 제목·URL에서 값을 자리표시자로
      되돌린다. 값은 해제될 때 덮어쓴다. [docs/credentials.md](credentials.md).
- [x] M4.2 agent-browser의 auth vault는 쓰지 않는다. 자리표시자로 대신한다.
- [x] M4.3 유출 경로 점검. CDP 결과·오류·이벤트, 접근성 트리, 추적을 켠 엔진
      stderr, 디스크는 확인했다(`verify-login.sh` 23개, 네이티브·WASIX). 콘솔은
      아직 내보내지 않고, 스크린샷은 아직 없으며, 의존성의 panic 메시지는 통제하지
      못한다. 악의적인 클라이언트는 한 글자씩 빼낼 수 있다(문서에 명시).

## 5. 미결 사항

- [x] **QuickJS를 패치할지 (M2.10).** 했다. rquickjs-sys를 빌드에 쓰는 파일만
      남겨 `vendor/rquickjs-sys`(4.4MB)에 두고 `[patch.crates-io]`로 바꿔 끼웠다.
      패치는 `vendor/patches/`에 diff로도 둔다. `Context::full` 문제(sysroot)도
      이제 같은 자리에서 고칠 수 있다.

- [ ] **wasixcc를 설치할지.** 설치하면 `Context::full`이 살아나고
      `performance.now()`가 동작하며 QuickJS 빌드 우회가 사라진다. 대신
      `~/.wasixcc`에 LLVM·binaryen을 받고 셸 rc 파일을 수정한다. 지금 상태로도
      JS 엔진은 쓸 수 있으므로 블로커가 아니다.
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
- **Chrome과 레이아웃을 비교하려면** `~/Library/Caches/ms-playwright`의 headless
  shell을 `agent-browser --executable-path`로 띄운다. 사용자의 Chrome 프로필을
  건드리지 않는다. 둘 다 `set viewport 800 600`으로 맞춘다.
- **Wasmer에서는 게스트의 stdin이 늘 터미널로 보인다.** 파이프로 넣어도 그렇다.
  그래서 브라우저는 WASI 타깃에서 터미널 검사를 하지 않고, `wasix-run.sh`가 호스트에서
  한다. 값 자체는 파이프로 제대로 전달된다. 또 Wasmer는 호스트의 환경 변수를 넘기지
  않으므로 `WHB_TRACE_CDP` 같은 것은 `wasmer run --env`로 준다.
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
