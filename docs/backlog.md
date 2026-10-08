# Backlog

작업 기록입니다. 이미 측정으로 확정한 사실과, 아직 남은 일을 나눠 적었습니다.
문서가 영어로 된 다른 파일들과 섞이지만, 이 문서는 작업용이라 한국어로 씁니다.

마지막 갱신: M0 완료, 밀린 정리 완료, M1 착수 직전.

## 1. 현재 위치

| 단계 | 내용 | 상태 |
|---|---|---|
| M0 | CDP 프로토콜 경로와 엔진 경계 확정 | 완료 |
| — | WASIX 네이티브 실행 (loopback, 검증 16건) | 완료 |
| — | 브라우저 샌드박스 loopback (E0·E1·E2) | 완료 |
| — | 브라우저 샌드박스 외부 통신 (E4·E5) | 완료 |
| — | QuickJS 실행 가능성 확인 | 완료 |
| M1 | 실제 엔진: HTML 파싱, DOM, 접근성 트리, 근사 레이아웃 | 착수 전 |
| M2 | JavaScript 엔진과 DOM 바인딩 | 착수 전 |
| M3 | 네트워크 이벤트, 쿠키·스토리지, 스크린샷 | 착수 전 |
| M4 | 자격증명을 메모리로만 다루기 | 착수 전 |

지금 상태로 동작하는 것은 다음뿐입니다. CDP를 말하고, 가짜 페이지 하나를
고정된 모델에서 답하며, 그 페이지를 WASIX 모듈로 패키징할 수 있습니다.
**실제 HTML은 아직 한 줄도 파싱하지 않습니다.**

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
| QuickJS는 WASIX에서 실행된다. `rquickjs` 0.14 + `full-wasi` + `bindgen`, 컨텍스트는 `Context::custom`으로 만든다 | [docs/quickjs.md](quickjs.md) |
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

- [ ] M1.1 `html5ever`로 HTML을 파싱해 실제 DOM 트리를 만든다.
      `engine-fake/src/dom.rs`의 고정 모델을 대체한다.
- [ ] M1.2 접근성 트리를 실제 DOM 위에서 계산한다. role과 name을 구하고,
      `ignored` 판정을 넣는다. `snapshot`이 곧 이것에 의존하므로 M1에서
      가장 값어치 있는 조각이다.
- [ ] M1.3 엔진이 직접 페이지를 가져온다. `std::net`으로 평문 HTTP를 열면
      네이티브와 WASIX 양쪽에서 동작하고, 브라우저 샌드박스에서는 E4·E5로
      확인한 대로 WISP가 자동으로 처리한다. **WISP를 인지하는 코드는 필요 없다.**
      HTTPS는 TLS를 게스트로 가져와야 하므로 별도 조각으로 둔다.
- [ ] M1.4 `taffy`로 블록·플렉스 레이아웃을 근사한다. 목표는 픽셀이 아니라
      `getBoundingClientRect`가 0이 아닌 사각형을 돌려주고 가시성을 판정하는
      것이다. 최소 CSS 캐스케이드(태그·클래스·인라인)가 함께 필요하다.
- [ ] M1.5 `engine-fake`의 PNG 렌더러를 실제 레이아웃에 연결한다. 현재는
      저장 방식 deflate라 결과가 큽니다. `miniz_oxide` 도입을 검토한다.
- [ ] M1.6 같은 캡처 방식으로 CDP 표면을 다시 측정한다. 실제 사이트를
      대상으로 어떤 메서드가 더 필요해지는지 확인한다.

### 4.3 M2 JavaScript (P0, 큼)

- [ ] M2.1 QuickJS를 엔진 모듈에 통합한다. `Context::custom` 경로를 쓴다.
      `wasixcc`를 설치하면 `Context::full`로 단순해진다 (§5 참조).
- [ ] M2.2 DOM 바인딩을 만든다. `document`, `window`, `querySelector`,
      `getComputedStyle`, `getBoundingClientRect`, `focus`, `scrollIntoView`,
      `elementFromPoint`가 필요하다. 앞의 다섯 개는 `agent-browser`가 실제로
      호출하는 것을 캡처로 확인했다.
- [ ] M2.3 `Runtime.evaluate`가 규칙표가 아니라 진짜 JS를 실행하게 한다.
      `engine-fake/src/eval.rs`를 삭제한다.
- [ ] M2.4 원격 객체 핸들을 관리한다. `Runtime.callFunctionOn`이
      `objectId`로 객체를 참조하므로 필요하다.
- [ ] M2.5 클릭 자동화를 연결한다. `Input.dispatchMouseEvent`와 히트테스트가
      만나야 비로소 `click`이 동작한다.
- [ ] M2.6 이벤트 루프를 만든다. `setTimeout`, 마이크로태스크, `Promise`가
      페이지 스크립트에서 흔히 쓰인다.

### 4.4 M3 네트워크와 저장소 (P1)

- [ ] M3.1 `Network.*` 이벤트를 낸다. `agent-browser`가 HAR과 `route`에 쓴다.
- [ ] M3.2 쿠키와 스토리지를 구현한다. **로그인 흐름에 필수다.**
- [ ] M3.3 요청 가로채기를 구현한다. `agent-browser network route`.
- [ ] M3.4 `Page.captureScreenshot`을 실제 렌더 결과에 연결한다.

### 4.5 M4 자격증명 (P1, 목표의 핵심)

- [ ] M4.1 자격증명을 메모리로만 받는 경로를 설계한다. 샌드박스 파일시스템과
      브라우저 스토리지 어디에도 쓰지 않는다.
- [ ] M4.2 `agent-browser`의 auth vault를 쓰지 않는다. 그 기능은 디스크에
      저장하므로 요구와 정면으로 충돌한다. 직접 주입한다.
- [ ] M4.3 비밀번호가 새지 않는 경로를 확인한다. 에러 메시지, 콘솔 로그,
      스크린샷, CDP 이벤트, 크래시 출력을 모두 본다.

## 5. 미결 사항

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
