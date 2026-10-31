# `conbus sync` 클라이언트 — 로컬 작업 계획

상태: **계획만.** 이 문서는 구현이 아니다. 동기화 코드, 버전 변경, 패키지는 없다.

Linux에서는 macOS 로그인 키체인, `lipo` / `codesign`, 시스템 신뢰 저장소를 확인할 수 없다. 구현과 패키징은 Mac에서 한다. 이 문서가 그 순서다.

기준 버전은 **0.2.8**. 따라 할 계약은 [RFD 0002](rfd/0002-metadata-sync.md)와 [RFD 0003](rfd/0003-security-design.md)이다. 릴리스 절차는 [release.md](release.md)다.

## 만들 것

0.2.9에 들어가는 것은 클라이언트 명령뿐이다.

- `conbus sync --join INVITE` — 초대 내용을 출력하고 아무것도 쓰지 않는다.
- `conbus sync --join INVITE --apply` — 그 뒤에야 디바이스 키와 설정을 쓴다. `install --apply`와 같다.
- `conbus sync` — 아직 보내지 않은 닫힌 UTC 날짜의 usage 행만 보낸다.
- `conbus sync --off` — 서비스에 닿지 못해도 로컬 키를 지운다. 로그는 남긴다. 그 다음 `conbus sync`는 아무것도 보내지 않는다.

이미 있는 것, 다시 만들지 않는다.

- `conbus usage --json --shareable` — 동기화가 보낼 행의 미리보기.
- `conbus usage --otlp URL` — 이 Mac의 컬렉터로 보내는 스파이크. 동기화가 아니다.

## 만들지 말 것

- 호스트, 피어, 원장. RFD 0003은 파일럿 세 곳과 월 $1k 서명 전에는 서비스를 호스트하지 않는다고 적는다. 그 게이트는 이 작업이 열지 않는다.
- RFD 0002 / 0003을 accepted로 바꾸는 것. 둘은 같이 수락되거나 같이 수락되지 않는다. 클라이언트만 초안 계약에 맞춰 구현한다.
- `conbus --off`. 떠나는 명령은 `conbus sync --off`다.
- Enterprise 해시 체인과 `conbus export --signed`. 구매자가 요구하기 전에는 만들지 않는다고 RFD 0003이 적는다.
- 워크스페이스 필드, `plan`, `automation_key`. RFD 0002에서 version 1 밖이다.
- 레코더 루프 안의 전송. 훅 예산은 500ms다. 보내기가 `poll` 루프를 막으면 기록이 멈춘다. 이 버전의 동작은 사람이 `conbus sync`를 실행하는 것이다. 하루에 한 번 레코더가 보내는 경로는 자식 프로세스로 분리할 설계가 따로 생기기 전에는 넣지 않는다.

## 구현 순서

`main`에서 이 계획 브랜치가 아닌 새 브랜치를 판다. 이 파일은 그대로 두고 구현 PR을 따로 연다.

1. RFD 0002의 “Commands (proposed, not built unless marked)”와 행 표, RFD 0003의 D1–D9, 그리고 0003 끝의 “Tests owed”를 구현 목록으로 고정한다. 표에 없는 필드는 보내지 않는다.
2. `crates/conbus/src/sync.rs`를 만든다. 이 모듈은 `SegmentLog`를 열지 않는다. `conbus_core::usage::SharedRow`만 받는다. 로그를 읽는 호출은 기존 `usage` 쪽에 둔다 (RFD 0003 D9).
3. 초대 JSON에 두는 필드는 D8이 이름 붙인 것만 한다. `org`, `org_name`, `origin`, `region` (`us` 또는 `eu`), `member`, `expires` (`YYYY-MM-DD`), `settings`. 설정은 `omit_harnesses`로 빼는 것만 인정한다. 그 밖의 설정 키는 무시하고, 행을 넓히지 않는다. `origin`은 `https://`만 허용한다. 사용자 정보, `?`, `#`이 있으면 거절한다. `--apply` 없이 출력만 할 때도 거절 이유는 보여 주되 쓰지 않는다. 만료된 초대는 `--apply`에서 거절한다.
4. 서명은 D1의 Ed25519다. 의존성은 `ed25519-dalek` 2.1, default features를 끄고 `alloc`과 `zeroize`만 켠다. `Signer` / `Verifier`는 그 크레이트가 다시 내보낸다. 시드는 `getrandom` 0.2로 32바이트를 만든다. 다이제스트는 `sha2` 0.10이다.
5. 개인키는 로그인 키체인에만 둔다. 서비스 `sh.conbus`, 계정 `sync-device-key`. `security -w`로 넘기지 않는다. 그러면 프로세스 인자에 키가 남는다. macOS 전용 의존성 `security-framework` 2.11의 `set_generic_password`, `get_generic_password`, `delete_generic_password`를 쓴다. 항목이 없으면 OSStatus `-25300`이다. Linux 빌드는 이 의존성을 포함하지 않고, 키를 쓸 수 없으면 `--apply`가 실패한다.
6. 상태 파일은 데이터 루트의 `sync.json`, 모드 `0600`이다. 디바이스 id, origin, 조직, 설정, 보낸 날짜와 revision과 본문 다이제스트, `pause_until`, 하루 요청 횟수만 둔다. 개인키 바이트는 파일에도 로그에도 없다.
7. 전송은 `/usr/bin/curl`이다. 인자는 `--config - --silent --show-error --max-time 30 --proto =https`뿐이다. URL, `Content-Type: application/json`, `Conbus-Signature`(본문 바이트에 대한 Ed25519 서명의 hex), 본문은 설정 문자열로 stdin에 넣는다. `http://`는 curl을 띄우기 전에 거절한다. 리다이렉트를 따라가지 않는다. 한 번에 하나의 origin, 초대에 적힌 그 URL만 POST한다. 경로를 덧붙이지 않는다. 본문으로 구분한다.
   - join: `invite`, `public_key`. 응답에서 `device`만 읽는다. 128비트, hex 32자.
   - batch: 봉투. 키는 `device`, `schema` (`1`), `day_closed`, `revision`, `rows`뿐이다.
   - leave: `device`, `leave: true`.
8. 배치 응답은 셋만 본다. `accepted`와 `revision`, `revoked`, `pause_until`. 다른 필드는 무시한다. 수락되기 전에는 그 날짜를 보낸 것으로 기록하지 않는다. `revoked`면 키와 상태를 지운다. `pause_until` 이전에는 보내지 않는다. 본문이 64행 또는 64KiB를 넘으면 그 날짜를 보내지 않는다. 디바이스당 하루 10요청을 넘으면 멈춘다. 행은 90일 창 안의 닫힌 날만 고른다. 날짜 D는 UTC로 D+1 00:00이 지난 뒤에만 보낼 수 있고, 그 뒤 최대 한 시간은 `SHA-256(device || day)`로 정한 지연이 지난 뒤다.
9. 같은 다이제스트는 다시 보내지 않는다. 본문이 바뀌면 revision을 1부터 올려 그 값만 보낸다. 열린 날과 오늘 자는 절대 보내지 않는다.
10. `--off`는 leave POST를 시도하고, 실패해도 키체인 항목과 `sync.json`을 지운다. 로그 경로는 남았다고 출력한다. 조직에 디바이스 삭제를 요청하면 영수증에 조직, 디바이스, 날짜 범위, 지워진 행 수, 마지막 백업 만료일이 온다는 문장까지 출력한다. 영수증을 대신 만들어 주지 않는다.
11. `conbus` 도움말과 README 명령 표에 세 형태를 넣는다. `doctor`에 동기화 줄을 넣는 것은 이 작업의 조건이 아니다.

## 아무 환경에서나 돌릴 테스트

`cargo test`로 고정한다. 키체인이 없어도 통과해야 한다.

- 봉투 키 집합이 `device`, `schema`, `day_closed`, `revision`, `rows`와 같다.
- 각 행의 키가 RFD 0002 표와 같다. 워크스페이스 키는 없다.
- 열린 날은 계획에 들어가지 않는다. 같은 본문은 한 revision만 나간다. 본문이 바뀌면 revision이 오른다.
- 응답의 모르는 필드는 설정을 넓히지 않는다.
- `pause_until` 전에는 전송 함수가 호출되지 않는다.
- 수락이 아닌 응답은 `sent`에 기록되지 않는다.
- `--off`가 키 저장소를 비운 뒤의 `sync`는 POST를 하지 않는다.
- curl 인자 목록과 `sync.json` 내용에 시드 hex가 없다.
- `http://` origin은 전송 함수를 부르기 전에 거절된다.
- `omit_harnesses`에 있는 하네스는 본문에 없다.

RFD 0003이 서비스에 요구하는 거절(오래된 revision, 같은 revision의 다른 본문, 창 밖의 날, 잘못된 서명, 폐기된 디바이스)과 삭제 영수증은 호스트가 없으므로 이 버전의 테스트가 아니다. 클라이언트는 수락하지 않은 응답을 보낸 것으로 기록하지 않으면 된다.

## Mac에서만 확인할 것

1. `--join INVITE --apply` 뒤에 키체인에 항목이 있다.

   ```bash
   security find-generic-password -s sh.conbus -a sync-device-key
   ```

2. `conbus sync --off` 뒤에 같은 명령이 실패하고, 이어서 `conbus sync`가 보내지 않았다고 출력한다.
3. 로컬 HTTPS로 join, batch, leave를 받아 서명이 본문 바이트와 맞는지 본다. 프로세스 목록에 시드가 없는지도 본다.
4. 패키지. `scripts/package.sh`는 두 Apple 슬라이스와 CHANGELOG 절이 있어야 한다.

   ```bash
   rustup target add aarch64-apple-darwin x86_64-apple-darwin
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   cargo build --release --locked -p conbus --target aarch64-apple-darwin
   cargo build --release --locked -p conbus --target x86_64-apple-darwin
   sh scripts/package.sh dist
   HOME=$(mktemp -d) CONBUS_BASE_URL="file://$PWD/dist" sh install.sh
   "$HOME/.local/bin/conbus" --version
   ```

   산출물은 `dist/conbus-macos-universal.tar.gz`, 그 `.sha256`, `dist/install.sh`다. `--version`은 `0.2.9`여야 한다.

## 버전과 README

구현 테스트가 통과한 뒤에만 올린다.

- `Cargo.toml`의 workspace `version`을 `0.2.9`로 하고 `cargo build`로 `Cargo.lock`을 맞춘다.
- `CHANGELOG.md`에 `## [0.2.9] — YYYY-MM-DD`를 넣는다. 이 절이 없으면 `package.sh`와 릴리스 워크플로가 실패한다. 적은 내용은 클라이언트 명령이 생겼다는 것과, 호스트는 여전히 없다는 것이다.
- README의 `0.2.8` 현재 버전 표시를 `0.2.9`로 바꾼다. 명령 표에 `conbus sync`를 넣는다. “무엇을 밖으로 보내는가”에, 참여한 뒤에 닫힌 날의 횟수만 초대에 적힌 origin으로 나가며 프롬프트는 로그에 남는다고 적는다. 동기화 서비스가 이미 떠 있는 것처럼 적지 않는다. 설치는 지금의 `install.sh` 한 줄을 유지한다.
- `docs/manual.md`와 `docs/manual.ko.md`의 “이 매뉴얼은 0.2.8” 첫 줄, `docs/www/index.html`의 버전 표시도 현재 릴리스를 말하면 `0.2.9`로 맞춘다. 0.2.8에 대한 과거 기록 문장은 그대로 둔다.
- RFD 0002의 명령 목록에서 세 명령을 built로 고친다. “No sync code exists”는 클라이언트가 0.2.9에 있고 서비스는 없다는 문장으로 바꾼다. 0003 첫머리도 같다. 수락 상태는 draft로 남긴다.

## 태그는 나중에

구현 PR이 `main`에 들어가고 CI가 초록이 된 뒤에만 태그를 단다. 태그는 공개 릴리스다.

```bash
git tag v0.2.9
git push origin v0.2.9
```

태그는 워크스페이스 버전과 같아야 한다. 게시 없이 패키지만 보려면 Actions에서 `release` 워크플로를 수동 실행한다. `workflow_dispatch`는 테스트와 패키지 아티팩트만 만들고 릴리스는 만들지 않는다.

## 끝

- `conbus sync`, `--join`, `--off`가 도움말에 있다.
- 위의 단위 테스트가 통과한다.
- Mac에서 키체인 생성과 `--off` 삭제를 확인했다.
- `dist/` 패키지의 `--version`이 0.2.9다.
- README가 그 명령과 “호스트는 없다”를 같이 말한다.
- `v0.2.9` 태그는 이 계획이 아니라, `main`에 올라간 구현에만 단다.
