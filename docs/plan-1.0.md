# conbus 1.0.0 계획 — v0.2.9에서 1.0.0까지

작성 2026-11-01 · 기준 `main` 74659b1 (v0.2.9) · 실행: Devin Fusion 13시간 + 운영자 승인 · 적대적 검토 1회 반영

**한 줄.** 구현을 전부 끝내 `v1.0.0-rc.1`을 만든다 → shadow와 canary를 그 RC로 한 번에 돌린다 → 통과하면 운영자 승인으로 `v1.0.0`을 자른다. 그 사이에 공개 정식 릴리스는 없다.

이 문서는 이 파일만 읽고 시작해도 되도록 썼다. 저장소 규칙은 `AGENTS.md`, 쌓인 지식은 `HANDOFF.md`에 있다. 두 파일이 이 문서와 어긋나면 **실제 코드와 명령 출력**이 이기고, 어긋난 문서는 고친다.

---

## 1. 1.0.0이 뜻하는 것

CHANGELOG 첫머리의 "until 1.0 a minor version may change the command line or the log format"이 끝나는 버전이다. 즉 1.0.0은 **로컬 제품의 안정성 약속**이다.

**1.x에서 깨지 않는 것 (stable)**

| 표면 | 약속 |
| --- | --- |
| 명령·플래그·종료 코드 | `install`, `uninstall`, `doctor`, `status`, `quota`, `log`, `usage`, `progress`, `backfill`, `run`, `go`, `hop`(수동), `daemon`, `hook`, `--version` |
| 기계용 출력 | `usage --json`, `usage --json --shareable`, `progress --json`, `log --json`. 키는 추가만 하고, 소비자는 모르는 키를 무시한다 |
| OTLP | RFD 0002 표의 메트릭 이름·단위·속성 |
| 로그 형식 | 1.x 빌드는 이전의 모든 레코드를 읽는다. 변경은 추가만 |
| 데이터 루트 | `~/.conbus` 구조와 권한, `install`/`uninstall` 왕복 |
| 훅 계약 | RFD 0001과 하네스별 fixture |
| 플랫폼 | macOS universal(Developer ID 서명·공증), Linux x86_64·aarch64 |

**experimental** (`--help`와 문서에 표시, 1.x 안에서 바뀔 수 있음): 자동 hop, `sync`, `policy`, `--profile`, `arena`, `summary`, `verdict`, `--inference-listen`.

**1.0.0에 넣지 않는 것**: 호스트·계정·결제(돈 게이트 뒤), 랜딩 배포·DNS, Copilot·Gemini 어댑터, 감사용 해시 체인, AI 협업 보고서, `MessageDisplay` 미러링, 채팅 UI.

---

## 2. 지금 상태 — 2026-11-01 실측

문서가 아니라 명령 출력으로 확인한 것들이다. HANDOFF의 일부는 이미 사실과 다르다.

| 사실 | 영향 |
| --- | --- |
| 저장소는 이미 **public**. `HANDOFF.md`, `docs/release.md`, `docs/fleet.md`, `docs/www/index.html`은 아직 private라고 적는다 | M0 |
| `docs/publication-review.md`가 재작성을 요구한 커밋(`ca4474f`~`ac42cca`)이 공개 GitHub에 그대로 있다 | 운영자 결정 O1 |
| Private vulnerability reporting 꺼짐(SECURITY.md는 그 경로로 제보하라고 안내), secret scanning·Dependabot 꺼짐, `main` 보호 없음 | M0 |
| v0.2.9 패키지는 ad-hoc 서명인데 `latest`다. 이 Mac은 아직 0.2.8. #68 수정은 태그 뒤라 어느 패키지에도 없다 | M0, M7 |
| **`devin/unlock`**: 2026-10-14 작성, 미푸시, 16커밋(Linux·policy·hook tokens·profiles·site, 설계 제약 개정). 지난 보고에서 "stale"로 분류했지만 실제로는 1.0의 큰 몫이다 | M1 |
| 시험 병합(`main`+`devin/unlock`): 문서 충돌 2개(`README.md`, `docs/manual.md`), 빌드 성공, 테스트 약 760개 중 진짜 실패 1개(`sync.rs`의 행 키 목록에 `tokens_*` 없음). `recorder.rs`의 다른 실패 1개는 `git archive`로 뽑아 `.git`이 없어서 생긴 것 | M1 |
| `stash@{0}`의 문서 5개는 `c5850e2`와 바이트까지 같다 | 흡수 후 삭제 가능(O8) |
| 벤더가 fixture보다 많이 앞서 있다(표 M2). 특히 OpenCode는 1.18.31 → 2.2.3이고 마지막 기록이 10/21이다. 어댑터가 살아 있는지 모른다 | M2 |
| 로그 96만 레코드, 9.7 GB, 세그먼트 약 1,380개. `encrypted_tools`를 켠 10/9 이후 주 1.2~4 GB, 그 전에는 주 0.1 GB 안팎 | M3 |
| **시작 복구 4.4 s(9/28) → 98.9 s(10/31)**, 이어서 fold 10.6 s. 지금은 재시작마다 약 110초 동안 훅이 응답을 못 받는다 | M3 |
| 읽기: `log --tail 1` 8.1 s, `usage --days 7` 12.7 s, `doctor` 22.4 s. 전부 전체 스캔 때문 | M3 |
| 드롭 7일 11,053건: 세션 없음 10,904건(Cursor `preToolUse`, 마지막 10/25), 바인딩 없음 146건, EPIPE 3건 | M2, M3 |
| 두 번째 유기적 hop(10/30): `automatic hop --apply … in 67571 ms`. 21.6분은 재현되지 않았지만 67.6 s도 길다 | M3 |
| `sync`의 curl 인자에 `-q`가 없어 `~/.curlrc`가 적용된다. RFD 0003 D5(리다이렉트 금지, 인증서 검증)를 파일 하나가 무너뜨릴 수 있다. 본문도 `data`로 보낸다 | M5 |
| `release.yml`은 `v*` 태그를 전부 정식 릴리스로 게시한다. 이대로 RC를 태그하면 `latest`가 RC가 되고, 서명 전 ad-hoc 패키지가 공개된다. `sign-release.sh`의 버전 검사는 `-rc.N`을 거부한다 | M5 |
| CI는 Rust 1.94.1, 이 Mac의 기본은 1.99.1(1.94.1 미설치). 1.99.1 clippy는 `conbus-core/src/usage.rs`에서 2건 실패 | M0 |
| **`install --apply`와 `uninstall --apply`는 `HOME`과 무관하게 `launchctl bootout gui/<uid>/sh.conbus.daemon`을 부른다**(라벨이 사용자 단위, `report.rs:398`, `uninstall.rs:328`). 버린 `HOME`에서 돌려도 라이브 레코더가 내려가고 새 바이너리가 진짜 `~/.conbus`로 뜬다. `CONBUS_SKIP_LAUNCHCTL=1`만이 막는다. `cargo test`는 자체적으로 켜므로 안전 | 5절, M7.0 |
| PR #63(Linux Cloud Agent 메모)은 `main`이 아니라 다른 브랜치로 머지됐다 | M1.4에서 흡수 |
| V1 집계표는 12행 모두 비어 있다. `devin/unlock` 문서는 "12회 인터뷰는 없을 것"이라며 V0–V1을 E1–E3 사다리로 대체한다. 저장소 공개(10/27 무렵)는 E1의 첫 항목이다 | O7 |

---

## 3. 결정

운영자가 달리 말하지 않는 한 이대로 간다.

| # | 결정 | 이유 |
| --- | --- | --- |
| D1 | 1.0.0은 로컬 제품이다. 호스트 게이트와 분리한다 | 호스트는 돈이 움직인 뒤다(`docs/saas-strategy.md`). 그 게이트를 1.0이 기다릴 이유가 없다 |
| D2 | `devin/unlock`을 기능별 PR로 흡수한다 | 이미 만들어졌고 테스트가 있으며 충돌이 적다. 제약 개정도 법적 제약은 그대로 두고, `CLAUDE_CONFIG_DIR`의 다계정 용도는 공식 문서에서 확인했다 |
| D3 | RFD 0002·0003은 draft로 두고 `sync`는 experimental로 낸다. 검증은 테스트 전용 reference 서버로 한다 | 수락은 운영자 결정이고, 없는 서비스 때문에 클라이언트 검증을 미룰 필요가 없다 |
| D4 | 중간 공개 릴리스 없음. `latest`는 서명한 0.2.9로 두고, RC는 prerelease다 | "shadow·canary는 구현 후 한 번에"와 맞는다. 검증 안 된 코드를 공개 설치 경로에 내보내지 않는다 |
| D5 | canary 교체는 **운영자 한 줄**로 시작한다. 13시간 실행은 교체하지 않는다 | 무인 대화 대부분이 이 훅을 지난다. 망가진 훅을 아무도 안 볼 때 띄우면 다른 작업까지 흔든다. 대기 비용은 명령 한 줄뿐이다 |
| D6 | `v1.0.0` 태그는 운영자 승인 뒤다 | 공개 약속이다 |
| D7 | 로그 `SCHEMA_VERSION`은 3을 유지한다. 필드 추가만 | 롤백이 쉬워진다. 올려야 한다면 근거를 기록하고, 0.2.8이 읽는 범위를 S1로 증명한다 |
| D8 | 읽기 가속은 파생 사이드카로 한다. SQLite·새 C 의존성 없음 | 세그먼트가 진실의 원천이라는 설계와 맞고, 지워도 다시 만들 수 있다 |
| D9 | 툴체인은 `rust-toolchain.toml` 한 곳에 고정한다(1.99.1) | 로컬과 CI가 다른 답을 내는 일을 없앤다 |
| D10 | RC와 개발 빌드는 canary 전까지 진짜 데이터 루트에 닿지 않는다 | M3.1 뒤로는 읽기 명령도 `cache/`를 쓴다. 항상 `CONBUS_HOME=<임시>`로 돈다 |

---

## 4. 순서와 시간

```
M0 기준선 → M1 unlock 흡수 → M2 벤더 계약 → M3 레코더 → M4 계약 동결 → M5 보안·공급망 → M6 문서 → M7 RC → S shadow
                                                                                               ↓
                                              (운영자 한 줄) canary 7일 → 판정 → (운영자 승인) v1.0.0
```

| 단계 | 예산 | 누적 | 주 담당 |
| --- | --- | --- | --- |
| M0 기준선 | 0:45 | 0:45 | Sidekick, Lead 확인 |
| M1 unlock 흡수 | 1:30 | 2:15 | Sidekick, 의미 충돌은 Lead |
| M2 벤더 계약 갱신 | 1:15 | 3:30 | Sidekick 실행, Lead 판독 |
| M3 레코더 신뢰성·성능 | 3:15 | 6:45 | **Lead 설계**, Sidekick 구현 |
| M4 계약 동결 | 1:15 | 8:00 | Lead |
| M5 보안·공급망 | 1:15 | 9:15 | **Lead** |
| M6 문서·site | 0:30 | 9:45 | Sidekick |
| M7 스크립트·RC | 1:00 | 10:45 | Lead |
| S1–S5 shadow | 1:15 | 12:00 | Lead 판독 |
| 보고서 | 0:30 | 12:30 | Lead |
| 여유 | 0:30 | 13:00 | |

**예산을 넘기면 자르는 순서** (위에서부터):

1. M6의 site
2. M3.8 `log compact` (`doctor`의 디스크 줄은 남긴다)
3. M1.5 profiles
4. M5.4 관리형 Mac 잔여
5. M3.6 드롭 사유 정리
6. M3.5 hop 단축 (단계별 시간 기록은 남긴다)
7. M3.4 RSS 개선 (측정값 기록은 남긴다)
8. M4.5의 Linux 부분
9. M2의 Grok·Cursor·agy (미확인으로 기록)

**자르지 않는 것**: M0.2~M0.5, M1.1~M1.4, M2의 Claude Code·Codex·OpenCode, M3.0~M3.3, M3.7, M4.1~M4.4, M4.5의 macOS 부분, M5.1, M5.3, M5.5, M5.6, M6의 사실 오류 수정, M7. aarch64 Linux도 자르지 않는다(S4가 이 Mac의 arm64 VM에서 쓴다).

T+9:00에 M4가 안 끝났으면 M6은 사실 오류 수정만 하고 M7로 간다. T+12:00에 M7이 안 끝났으면 RC를 자르지 않고 마지막 초록 `main`에서 멈춘 뒤 보고한다. 구현이 끝나지 않으면 shadow·canary도 시작하지 않는다.

---

## 5. 모든 PR의 공통 규칙

- **브랜치**: `origin/main`에서 새로 판다. 이름은 `devin/m1.2-tokens` 형식. PR 하나에 한 가지.
- **동작 변경은 실패하는 테스트부터.** 그 테스트가 먼저 실패하는 걸 본 뒤에 고친다.
- **게이트**: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked --no-fail-fast`. CI의 `gate`(그리고 M1.4 뒤로 `linux`)가 초록이어야 머지한다.
- **라이브 보호**: 모든 수동 실행에 `CONBUS_SKIP_LAUNCHCTL=1`과 `CONBUS_HOME=<임시>`. 버린 `HOME`만으로는 라이브 레코더를 막지 못한다(2절). 소켓 경로 104바이트 제한 때문에 임시 루트는 `/tmp/cb-...`처럼 짧게.
- **머지**: `gh pr merge N --repo deximple/conbus --merge --delete-branch`. 저장소 이력이 머지 커밋이다. `--admin`, `--auto` 금지.
- **진행표**: 마일스톤의 마지막 PR에 함께 넣어 **마일스톤당 한 번** 갱신한다. 진행표만 고치는 PR을 만들지 않는다. 세부 진행은 저장소 밖 상태 파일에 쓴다.
- **되돌리기**: 머지 커밋을 `git revert -m 1`. M0.5·M0.6과 RC 게시를 빼면 이 계획의 구현 단계는 공개 저장소 안에서 끝나고, 라이브 레코더에 닿지 않는다.
- **커밋 메시지**: 영어 conventional commit, 동작을 문장으로(`fix: an unjoined conbus sync does not scan the log`). AI 표시 트레일러 없음.
- **저장소 파일은 그 파일의 언어를 따른다.** CHANGELOG는 사용자에게 보이는 PR마다 `[Unreleased]`에 적는다.
- **저장소는 public이다.** 푸시는 곧 공개다. 홈 경로, 비공개 프로젝트 이름, 쿼터 수치, 라이브 bus id와 세션 id는 커밋과 PR 본문에 넣지 않는다. fixture는 경로를 `/Users/USER`로 바꾸고 프롬프트는 고정 문구만 쓴다. 푸시 전에 `scripts/check-public-safe.sh`(M0.3).
- **AGENTS.md의 비협상 제약은 그대로다**: 벤더 토큰을 읽지 않는다, 벤더 바이너리를 고치지 않는다, 사람 사이에 자격·쿼터를 묶지 않는다, Codex `trusted_hash`를 쓰지 않는다, 라이브 경로에서 벤더 세션 파일을 파싱하지 않는다, 사용자가 연 Codex 스레드에 주입하지 않는다, `~/Downloads`를 읽지 않는다.
- **행 필드**: RFD 0002 표에 없는 키는 `SharedRow`에 넣지 않는다.
- **버전 숫자는 M7까지 건드리지 않는다.**

---

## 6. 구현

### M0 — 기준선 (0:45)

| ID | 할 일 | 완료 조건 |
| --- | --- | --- |
| M0.1 | 작업 트리를 만들고 `main` 그대로 3개 게이트를 돈다. 테스트 수를 기록 | 숫자가 상태 파일에 있다 |
| M0.2 `devin/m0-toolchain` | `rust-toolchain.toml`(1.99.1, rustfmt·clippy). `ci.yml`·`release.yml`이 같은 값을 읽게 한다. `rust-version`과 README의 "Rust 1.94+" 갱신. `conbus-core/src/usage.rs`의 `sort_by_key` 2건 수정 | 로컬과 CI가 같은 툴체인, 초록 |
| M0.3 `devin/m0-plan` | 이 문서를 `docs/plan-1.0.md`로 커밋한다(끝에 진행표). `.gitignore`에 `.DS_Store`. `scripts/check-public-safe.sh`: 추적 파일에서 `/Users/<이름>/`을 찾되(허용: `USER`, `user`, `x`, `you`, `me`, `admin`, `someone`), `CONBUS_PRIVATE_STRINGS` 파일이 있으면 그 문자열도 찾는다. CI에 넣는다 | **현재 트리에서 초록**, 일부러 넣은 문자열은 잡는다 |
| M0.4 `devin/m0-public-truth` | "private" 서술을 현실로: `HANDOFF.md` "Where things are"와 "Waiting on the operator", `docs/release.md`, `docs/fleet.md`, `docs/www/index.html`(0.2.7 표기 포함). `docs/publication-review.md`에는 공개됐고 재작성은 안 됐다는 사실을 쓴다. HANDOFF에 V0 통과(10/9)와 두 번째 hop(67.6 s)을 기록. HANDOFF의 "marker makes the hook exit before reading stdin"은 틀렸다(`main.rs`가 먼저 stdin을 읽는다). 고친다 | `grep -rnE '\*\*private\*\*\|is private\|until the repo is public' HANDOFF.md docs/` 결과가 없거나 과거 서술뿐 |
| M0.5 | 저장소 설정(`gh api`): private vulnerability reporting 켜기, Dependabot alerts, secret scanning·push protection, `main` 보호(필수 체크 `gate`, `strict: false`, 리뷰 요구 없음, `enforce_admins: false`, force push 금지). M1.4 뒤 `linux` 추가 | `gh api`로 읽어 확인, 상태 파일에 기록 |
| M0.6 | v0.2.9를 `scripts/sign-release.sh`로 서명·공증하고 `--upload`. 키체인 프롬프트로 멈출 수 있으니 15분 제한, 백그라운드로 돌리고 기다리지 않는다. 실패하면 운영자 목록으로 넘긴다 | 내려받아 `TeamIdentifier=4UN6WC576K`, 또는 운영자 목록에 기록 |

### M1 — `devin/unlock` 흡수 (1:30)

`devin/*` 브랜치는 다른 작업 트리에 물려 있어 체크아웃할 수 없다. 건드리지 말고 **커밋을 새 브랜치로 체리픽**한다. 가져오지 않는 것: `0ecc37d`(빈 스냅샷), 머지 커밋, `2ebe4a1`(내용만 HANDOFF에 반영).

| ID | 가져올 커밋 | 할 일 | 완료 조건 |
| --- | --- | --- | --- |
| M1.1 `devin/m1.1-constraints` | `c5850e2` | AGENTS.md·HANDOFF·RFD 0002·saas 문서. AGENTS의 명령 목록에 `usage`, `progress`, `sync`, `policy`도 넣는다. 전략 문서에 현재 사실(공개 완료, V0 통과)을 덧붙인다. 날짜는 바꾸지 않는다(O7) | CI 초록 |
| M1.2 `devin/m1.2-tokens` | `f2a2ab8`, `314c3ee` | **의미 충돌 수정**: `crates/conbus/src/sync.rs`의 행 키 테스트와 목록에 `tokens_*`. RFD 0002 행 표·OTLP 표 일치(`c5850e2`가 이미 표를 넣었다). `schema`는 1 유지(서비스가 없었으므로 draft 개정) | `--shareable` 키 = RFD 표 = `sync` 봉투 행 키 |
| M1.3 `devin/m1.3-policy` | `a694c2a` | `conbus policy`와 `PreToolUse` 판단. 기본 꺼짐, 파싱 실패 시 아무것도 결정하지 않고 `doctor`가 말한다는 점을 테스트로 고정 | 꺼진 상태에서 동작 변화 0 |
| M1.4 `devin/m1.4-linux` | `f6fe2a9`, `030030d` | systemd user unit, `install.sh`, `package.sh`, CI `linux` 잡, 릴리스 Linux 잡. `sync`는 Linux에서 컴파일되고 `--apply`가 키 저장소 없음을 명확히 거절한다. PR #63의 메모 중 유효한 것 반영. **aarch64 추가**(`ubuntu-24.04-arm`, `install.sh` 분기). 릴리스는 musl 정적 빌드 우선, 테스트가 실패하면 `ubuntu-22.04` gnu로 물러서고 glibc 하한을 문서에 적는다 | `linux` 잡 초록, `main` 보호에 `linux` 추가 |
| M1.5 `devin/m1.5-profiles` | `b34fe3d`, `d51164a` | experimental로 표시. 자동 hop이 프로필 사이를 옮기지 않는다는 테스트 유지 | 왕복 테스트 초록 |

M1의 끝: `git cherry -v main devin/unlock`과 파일별 대조로 남은 차이가 site와 의도한 문서뿐임을 확인하고 진행표에 설명한다. `README.md`·`docs/manual.md` 충돌은 **`main`의 새 구조를 기준으로** 새 내용을 넣어 푼다. `site/`는 M6이다.

### M2 — 벤더 계약 갱신 (1:15)

훅 실패는 조용하다. 그래서 실제 벤더로 확인한다. fixture가 낡았다는 것 자체가 1.0의 결함이다.

| 하네스 (이 순서) | fixture 기준 | 설치 버전 |
| --- | --- | --- |
| Claude Code | 2.1.241, statusline 2.1.263 | 2.1.316 |
| Codex | 0.152.1 | 0.166.0 |
| OpenCode | 1.18.31 | 2.2.3 |
| Grok | 1.0.3(문서) | 1.0.71 |
| Cursor | 3.19 | `cursor-agent` 2026.10.19 (IDE는 이 Mac에 없다) |
| agy | 1.1.27 usage | 1.4.12 (usage fixture만 갱신) |

**방법**: 하네스마다 버리는 체크아웃을 만든다. 훅 명령은 개발 빌드 `conbus hook --harness X`를 `tee`로 감싸 원문을 캡처하고, `CONBUS_HOME=/tmp/cb-m2`를 쓴다. 벤더는 `CONBUS_INTERNAL_SESSION=1`로 띄워 전역 훅을 침묵시키고(라이브 로그 오염 방지), 프로젝트 훅 명령은 `env -u CONBUS_INTERNAL_SESSION`으로 시작한다. 프롬프트는 "reply with ok"처럼 고정된 짧은 문구만, 하네스당 세션 3개까지.

- **Claude Code**: `claude --settings <파일> -p`로 훅을 주입한다. 신뢰 안 된 폴더의 프로젝트 설정은 안 읽힐 수 있다.
- **Codex**: TUI를 pty로 몬다(본문과 Enter를 2초 띄운다. `codex exec`는 이전에 멈췄으니 먼저 시험). 신뢰 대화상자에 전역 훅이 함께 보이면 신뢰하지 말고 미확인으로 적는다.
- **Grok**: 프로젝트 사본은 `--harness grok`로 등록한다. Grok은 Claude 형식 훅도 돌리는데, `--harness claude_code` 사본은 `grok_echo`로 침묵해 아무것도 안 남는다. 에코 가드 확인은 `repeated`가 아니라 **`tee` 타임스탬프**로 한다.

**답해야 할 질문**

1. OpenCode 2.x가 플러그인을 여전히 부르는가? 10/21 이후 0건은 안 쓴 것인가, 망가진 것인가.
2. 각 하네스의 이벤트 집합·필드가 `tests/fixtures/*_contract.md`와 같은가? 다르면 실패 테스트 → 어댑터 수정.
3. Grok 에코 가드가 1.0.71에서도 맞는가.
4. Cursor: 토큰 필드, 턴 이중 기록, **worker 세션 payload**. 재현되면 세션 키를 찾아 고치고, 안 되면 알려진 한계로 적는다.
5. `policy`의 deny를 벤더가 실제로 지키는가(하네스별 표). Codex는 `ask`를 거절한다고 기록돼 있다.

**산출물**: 새 fixture와 계약 문서(가림 처리), `docs/support-matrix.md`(플랫폼 × 하네스 × 확인 버전 × 기능), 드리프트 수정 PR.

**시간 제한**: 하네스당 15분. 로그인이 없거나 멈추면 매트릭스에 "미확인(이유)"로 적고 넘어간다. **로그인을 시도하지 않는다.**

**완료 조건**: `real_payloads` 테스트가 새 fixture로 초록, 매트릭스의 모든 칸이 "확인 버전" 또는 "미확인(이유)".

### M3 — 레코더 신뢰성·성능 (3:15)

| 항목 | 지금 (96만 레코드, 9.7 GB) | 1.0 목표 |
| --- | --- | --- |
| `log --tail 1` | 8.1 s | ≤ 0.2 s |
| `usage --days 7` | 12.7 s | ≤ 2 s |
| `doctor` | 22.4 s | ≤ 3 s |
| 시작 → serving(실제 재시작: 서빙 → 훅 기록 몇 개 → TERM → 재시작) | 약 110 s | ≤ 5 s |
| 재시작 중 훅 유실 | 0.2.7에서 5 s 창에 5건, 지금 창은 110 s | 0 |
| 레코더 RSS | 340 MB | ≤ 256 MB |

**목표를 못 맞추면**: 시간 박스 안에서 고치고, 그래도 안 되면 측정값을 적고 다음으로 간다. 숫자를 지어내거나 측정을 생략하지 않는다.

| ID | 할 일 | 완료 조건 |
| --- | --- | --- |
| M3.0 `devin/m3.0-bench` | `scripts/bench-corpus.sh OLD NEW ROOT`: APFS 클론(`cp -cR`) 위에서 위 표를 잰다. 말뭉치는 커밋하지 않는다. CI용으로는 합성 로그 테스트: `--tail`이 옛 세그먼트를 열지 않음을 **열린 파일 수**로 단언(시간 단언 금지) | 0.2.8 기준 숫자가 재현된다 |
| M3.1 `devin/m3.1-sidecar` | **닫힌 세그먼트**(이름상 최신이 아니고, 사이드카에 적힌 바이트 길이와 파일 길이가 같은 것)마다 파생 사이드카 `<root>/cache/`: 시간 범위 + 본문을 뺀 메타 레코드. 쓰기는 옆에 쓰고 rename, 모드 0600, 지워도 다시 만든다. 본문이 필요 없는 모든 읽기는 `SegmentLog::scan_each`와 같은 fold 코드로 사이드카를 읽는다(호출처는 `grep -rn 'SegmentLog::scan' crates/`: `usage`, `report`, `verdict`, `progress`, `hop`, `summarise`, `daemon`). `log --tail N`은 뒤에서 읽는다. `CONBUS_NO_CACHE=1`이면 전체 스캔. **`<root>/cache/`만 쓰는 명시적 빌드 명령**을 둔다(canary preflight가 쓴다) | fixture 로그와 실제 말뭉치 클론에서 사이드카 결과와 전체 스캔 결과가 **행 단위로 같다**. 사이드카를 지워도 결과 같음. 길이가 바뀐 세그먼트의 사이드카는 무효 |
| M3.2 `devin/m3.2-start` | 레코더 fold를 사이드카로. `daemon --prepare`는 체크포인트 이후의 세그먼트와 새 벤더 전사만 본다. "A reconstruction never outranks what the recorder saw", twin·split 테스트 유지 | 말뭉치 클론에서 시작 → serving ≤ 5 s(첫 빌드 제외, 첫 빌드 시간 별도 기록) |
| M3.3 `devin/m3.3-spool` | 아래 "스풀 설계" | 실패 먼저: 1,000개 훅 폭주 중 재시작 테스트에서 **유실 0**, 로그에 남은 중복은 `usage`가 한 번으로 세고 건수를 기록. 보존 수준별 테스트, 상한 초과 테스트, 레코더 없는 머신 테스트 |
| M3.4 `devin/m3.4-rss` | RSS의 정체를 재고 본문 크기에 비례하지 않게 한다 | 목표 또는 측정값 기록 |
| M3.5 `devin/m3.5-hop-time` | 자동 hop 줄에 단계별 시간을 넣고, 가짜 벤더 바이너리로 재현해 가장 큰 단계를 줄인다(`hop.rs`의 전체 스캔이 유력). 자동 hop은 experimental 유지 | 단계별 시간이 `daemon.log`에 있다 |
| M3.6 `devin/m3.6-drops` | `doctor`의 drops를 사유별로 나누고 뜻을 붙인다. "바인딩 없음"의 원인을 찾아 고치거나 분류한다 | 사유마다 설명이 있다 |
| M3.7 `devin/m3.7-old-v` | `v`1·2·3 황금 세그먼트 fixture. "older `v`는 현재처럼 읽는다"를 문서화된 규칙과 테스트로. **1절의 로그 약속을 시험하는 유일한 테스트다** | `scan` 결함 0 |
| M3.8 `devin/m3.8-compact` | `doctor`가 로그 크기와 여유 디스크를 말하고 10% 아래면 경고(**필수**). `conbus log compact --before DAY [--apply]`: 닫힌 세그먼트에서 본문을 빼 원자적으로 교체, 사이드카 무효화, 기본은 미리보기(**잘라도 되는 부분**) | `doctor` 줄 테스트, compact는 전후 `usage` 동일 |

**스풀 설계 (M3.3)** — 정확히 한 번을 약속하지 않는다. 유실을 0으로 만들고 중복은 세는 쪽에서 흡수한다. `client.rs`의 "빈 응답 뒤 재시도 금지" 주석 그대로, 응답 시간 초과는 이미 기록됐을 수 있기 때문이다.

- **언제**: 연결 거부, `Orphaned`, 프로토콜 불일치, 응답 시간 초과. 훅은 스풀에 쓰고 평소의 침묵 JSON으로 즉시 답한다. 훅 예산 500 ms, "항상 유효한 JSON과 exit 0" 유지.
- **`hook_id`**: 훅이 만든 고유 id를 `Request`에 추가하고(`PROTOCOL_VERSION` 올림) 레코드에 추가 필드로 남긴다. 레코더는 기억하는 id를 다시 쓰지 않는다. 재시작으로 기억을 잃어 로그에 둘 남으면 `usage`가 한 번으로 센다(`repeated`와 같은 원칙).
- **어디**: `<root>/spool/`, 파일 0600, 항목당 1 MiB, 총량 16 MiB. 넘으면 지금처럼 드롭으로 기록.
- **보존**: 스풀에 쓸 때 적용한다. `ephemeral`은 쓰지 않는다. 기본·`encrypted_tools`는 툴 본문 없이(본문이 생략됐다는 표시와 함께), `full`은 그대로.
- **동의**: **이 루트에서 레코더가 한 번이라도 돈 적이 있을 때만**(`daemon.build` 존재) 스풀한다. 관리형 Mac에서 사람이 `install --apply`를 하기 전에는 지금처럼 기록하지 않는다는 `docs/fleet.md`의 약속을 지킨다. `uninstall --apply`는 스풀을 지우고 그렇게 말한다.
- **순서**: `prepare`와 `restore`가 끝난 뒤 스풀을 비우고 `bind`한다. `bind` 뒤 한 번 더, 이후 주기적으로 비운다. 복구 중의 훅은 연결 거부로 곧장 스풀로 간다(500 ms를 기다리지 않는다). 빌드 스탬프는 지금처럼 복구 전에 쓴다. `doctor`는 "시작 중"을 "죽음"과 구별한다.
- **불변식**: 수집도 반드시 writer 하나를 지난다. `ts`는 쓰기 순서를 유지하고 원래 시각은 추가 필드로 남긴다.

AGENTS.md의 "SQLite는 파생" 항목에 사이드카와 스풀을 추가해 적는다.

### M4 — 계약 동결 (1:15)

| ID | 할 일 | 완료 조건 |
| --- | --- | --- |
| M4.1 `devin/m4.1-stability` | `docs/stability.md`: 1절 표를 약속으로. 제거는 2.0에서만, experimental 정의. `--help`에 표시 | 문서와 `--help` 일치 테스트 |
| M4.2 `devin/m4.2-cli` | 명령별 `--help` 황금 테스트. 종료 코드 규칙(0 성공, 1 실패, 2 사용법)을 정하고 어긋난 곳을 지금 고친다 | 표 기반 테스트 |
| M4.3 `devin/m4.3-json` | `usage --json`, `--shareable`, `progress --json`, `log --json` 키 집합 고정 | 키 추가 시 테스트가 알린다 |
| M4.4 `devin/m4.4-log-format` | `docs/log-format.md`: 봉투, 이벤트 종류, 추가 규칙, 롤백 규칙. `PROTOCOL_VERSION` 의미와 구버전 동작 | 문서의 필드 = `record.rs` |
| M4.5 `devin/m4.5-upgrade` | 0.2.8 상태 fixture(설정 파일, env 있는 plist, 플러그인, 세그먼트) → `install --apply` 멱등 → env 보존 → `uninstall --apply` 왕복. Linux systemd 동일 | 왕복 테스트 초록 |
| M4.6 `devin/m4.6-rfd` | RFD 0001을 Accepted로. RFD 0002·0003은 draft 유지, `sync` experimental 명시 | 문서 일치 |

### M5 — 보안·공급망 (1:15)

| ID | 할 일 | 완료 조건 |
| --- | --- | --- |
| M5.1 `devin/m5.1-curl` | `curl_args`(`sync.rs`): `-q`를 **첫 인자로**, `--tlsv1.2`, 리다이렉트 금지 고정, `data-binary`. 적대적 `~/.curlrc` 테스트 | 인자 목록 테스트, reference 서버에서 서명이 전송 바이트와 일치 |
| M5.2 `devin/m5.2-reference` | 테스트 전용 reference 서버(`tests/support/`, 바이너리에 안 들어감): RFD 0003 거절 규칙. join·batch·leave·revoked·`pause_until`. TLS는 macOS `CURL_CA_BUNDLE`을 먼저 시험하고, 안 되면 Linux CI 컨테이너에서 임시 CA로. **신뢰 저장소를 건드리지 않는다** | HANDOFF의 "서비스가 없어 미검증" 3건 중 서명·전송 해소 |
| M5.3 `devin/m5.3-inference` | `--inference-listen`은 루프백 외 주소를 거절. `SECURITY.md`·보안 문서 갱신 | 실패 테스트 → 통과 |
| M5.4 `devin/m5.4-managed` | `doctor`가 관리형 설정을 읽어 사실대로 말한다. `install --managed`가 root 외 쓰기 가능 바이너리를 경고. 경로 주입으로 테스트 | 테스트 |
| M5.5 `devin/m5.5-release` | **릴리스 경로**: `-` 포함 태그 → `--prerelease`, 아니면 `--draft`. 서명 후 `gh release edit --draft=false --latest`. `latest`가 ad-hoc을 가리키는 창을 없앤다. `sign-release.sh`의 버전 검사(30~34행)를 `v<semver>(-rc\.[0-9]+)?`로 넓힌다. `cargo-deny`, SBOM(CycloneDX), `actions/attest-build-provenance`는 **릴리스·드라이런과 주간 스케줄에서만**(PR CI를 느리게 하지 않는다). `install.sh`는 macOS에서 `TeamIdentifier`를 확인(기본 저장소 + 0.2.6 이상). `docs/release.md` 갱신 | `workflow_dispatch` 드라이런 초록, 산출물 확인 |
| M5.6 | Lead가 M1.3·M3.3·M5 전체를 보안 관점으로 읽는다. `SECURITY.md` 지원 버전, `docs/security-packet.md` 현행화(의존성 수, sync 존재, 스풀, 알려진 한계) | CRITICAL·HIGH 0, 지적은 진행표에 |

### M6 — 문서·site (0:30)

- README, `docs/manual.md`, **`docs/manual.ko.md`**(unlock이 갱신하지 않았다), `beginner-manual`, `fleet`, `hop-demo`를 현재 기능으로. HANDOFF의 "Next task"와 "Still open" 재작성, `docs/self-improve.md` 기록 추가.
- `scripts/check-docs.sh`(CI): 문서의 버전 표기 = 워크스페이스 버전, `--help`의 모든 명령이 README 표와 양쪽 매뉴얼에 있다.
- `CONTRIBUTING.md`(짧게), 이슈 템플릿(프롬프트·경로를 붙이지 말라고 안내).
- **site**(`devin/m6-site`, 커밋 `509ca47`, `fb54b33`, 가장 먼저 자를 항목): 코드만 흡수. 랜딩을 `site/public/` 하나로 합치고 `docs/www/`를 정리하며 문구의 사실을 다시 확인한다. `site/**` 경로 필터로 vitest CI(`npm test`). **배포하지 않는다.**

### M7 — 스크립트와 RC (1:00)

**M7.0 `devin/m7.0-scripts`** (0:30)

- `scripts/shadow-replay.sh OLD NEW CLONE`: S1 비교를 재현 가능하게.
- `scripts/canary.sh {preflight,status,verdict,start,restart,rollback}`: 7절대로. **진짜 교체는 진짜 `HOME` + `CONBUS_SKIP_LAUNCHCTL` 없음 + `CONBUS_CANARY_REHEARSAL` 없음이 모두 맞을 때만** 일어난다. `CONBUS_CANARY_REHEARSAL=1`이 있으면 무조건 리허설인데, 그것도 진짜 `HOME`에서는 어떤 파일도 건드리기 전에 거부한다(리허설은 임시 `HOME`에서만). 금지 시간대는 `CONBUS_CANARY_BLACKOUT` 환경 변수로 받는다(값은 저장소에 넣지 않는다). 형식은 `HH:MM-HH:MM`(매일, 자정 넘김 허용), `[요일] HH:MM-HH:MM [Zone]`(`Mon-Fri 09:00-15:30 Asia/Seoul` 같은), `<epoch>-<epoch>` 셋 중 하나다. 어느 것으로도 파싱되지 않는 값은 금지 시간대로 간주해 닫힌다(안전 쪽 실패).
- 임시 `HOME`에서 예행 연습한다. 리허설 스크립트의 가짜 바이너리는 `install --apply`에도 답하므로 실제 모드 분기까지 시험한다.

**RC**

1. **진입 조건**: M0~M6의 필수 항목 완료, `main` 초록.
2. PR: 버전 `1.0.0-rc.1`, `Cargo.lock`, CHANGELOG `## [1.0.0-rc.1] — YYYY-MM-DD`.
3. `release` 워크플로 드라이런 초록 뒤 `v1.0.0-rc.1` 태그. prerelease로 게시되는지, `latest`가 그대로인지 확인.
4. 서명·공증 후 `--upload`(백그라운드, 기다리지 않는다).
5. 버린 `HOME`에 설치: `HOME=$(mktemp -d) CONBUS_BASE_URL="file://$PWD/dist" sh install.sh` (`gh`는 임시 HOME에서 로그인을 잃는다). `--version` = `1.0.0-rc.1`.
6. **이 시점부터 `main`은 동결**: shadow·canary에서 나온 수정만 들어간다.

---

## 7. Shadow + Canary — 구현이 끝난 뒤 한 번에

같은 RC로 이어서 돈다. 단계별로 쪼개 여러 번 하지 않는다. 결함이 나오면 고쳐서 `rc.N+1`로 **실패한 단계부터** 다시 한다. 레코더 경로를 고쳤으면 canary 시계도 다시 시작한다.

### Shadow — 라이브에 영향 없음 (13시간 실행 안에서)

증거는 저장소 밖 실행 디렉터리에 둔다.

| ID | 내용 | 통과 조건 |
| --- | --- | --- |
| S1 말뭉치 재생 | **먼저 손대지 않은 클론**에서 배포본 0.2.8과 RC의 읽기를 비교: `usage --json`(전체 기간), `progress --json`, `verdict`, 레코드 수, `doctor`의 수치 줄. 쓰는 중이라 잘릴 수 있는 최신 세그먼트는 비교에서 뺀다. **그다음 두 번째 클론**에서 RC 레코더를 띄워 성능표를 재고, RC가 쓴 레코드를 0.2.8이 읽는지(롤백) 본다. 레코더 시작은 진짜 벤더 전사를 읽어 클론에 채운다. 클론이 두 개인 이유다. `encrypted_tools`를 켠다면 `CONBUS_KEK_HEX`로 진짜 `log-kek`와 키체인 대화상자를 피한다 | `tokens_*`와 문서화된 차이 외 **행 단위 동일**, 읽지 못한 줄 0, 성능표 기록, 롤백 안전 또는 새 이벤트 목록 명시 |
| S2 실제 벤더 나란히 | 버린 체크아웃에 RC 프로젝트 훅(`CONBUS_HOME=/tmp/cb-s2`), 전역 훅은 라이브 그대로(`CONBUS_INTERNAL_SESSION` 없이, 벤더에는 `CONBUS_HOME`을 넘기지 않고). 하네스당 짧은 세션 1개. 같은 `vendor_session_id`의 이벤트를 두 기록기가 어떻게 남겼는지 비교. 이 세션 몇 개는 라이브 로그에 남는다 | 이벤트 종류·수 동일(차이는 설명). 벤더 쪽에 hook 오류 없음 |
| S3 고장 주입 | 임시 루트의 RC에서: 폭주 중 재시작, 잘린 꼬리, 소켓 거부, 프로토콜 불일치, 손상 JSON, 레코더 없음 | 훅이 항상 유효한 JSON + exit 0, 유실 0(상한 초과는 드롭 기록) |
| S4 Linux | Colima VM(`conbus-rc` 프로필, arm64, 끝나면 삭제, **`default` 프로필은 건드리지 않는다**): `install.sh` → systemd user unit → fixture 훅 → `doctor` → 재시작 → `uninstall` | 왕복 통과. `colima ssh`에 user bus가 없으면 `loginctl enable-linger`와 `XDG_RUNTIME_DIR`로 시도하고, 그래도 안 되면 레코더를 직접 띄워 왕복만 확인한 뒤 "systemd 재시작 미검증"으로 기록. 실제 벤더 로그인이 없으므로 "Linux 실벤더 미검증"을 문서에 명시 |
| S5 sync 왕복 | reference 서버로 `--join --apply` → `sync` → `--off`, **한 빌드로만**(ad-hoc 재빌드는 키체인 신원이 바뀌어 대화상자를 부른다). 이 Mac의 실제 키체인에 `sync-device-key`가 생겼다가 지워지는지. `log-kek`는 건드리지 않는다. 대화상자가 뜨면 제한 시간 뒤 운영자 목록으로 | 항목 생성·삭제 확인, 프로세스 인자에 시드 없음, 이후 `sync`가 POST 안 함 |

### Canary — 운영자가 시작 (7일)

**시작**: `sh scripts/canary.sh start` (실행기의 환경이 아니라 운영자의 셸에서)

1. `preflight`: S1~S5 증거 존재, 서명본 sha 대조, `TeamIdentifier`, 롤백 복사본, plist 백업, 금지 시간대 확인. **새 바이너리로 라이브 루트에 사이드카를 미리 빌드한다**(`cache/`만 쓴다). 안 그러면 첫 시작이 수 분 걸려 아래 확인이 매번 실패한다.
2. `mv -f conbus.new conbus && conbus install --apply` (kickstart 아님, HANDOFF 참고).
3. 자동 확인: build = daemon build, `CONBUS_CANARY_SERVING_LIMIT`(기본 30초) 안에 새 pid가 serving, 훅 왕복(`hooks` 행이 전부 `runs`), **이전에 없던 드롭 사유 0 — 단, 교체 창에 불가피하게 생기는 두 사유는 예외로 세어 `canary.json`의 `swap_window_drops`에 남긴다**. 하나라도 실패하면 **자동 롤백**. 시작→serving 5초 목표는 롤백 조건이 아니라 `verdict` 판정 항목이다(현재 측정치가 5초 위라 매번 되돌리는 셈이 된다).

**첫 교체의 유실이 불가피한 이유**: `mv`와 레코더 정지 사이에 돈 새 훅(프로토콜 v6)은 옛 0.2.8 레코더(v5)에 거절당한다. 0.2.8의 `daemon.build`에는 보존 수준(`retention`)이 없어 훅이 스풀 동의를 얻지 못하고 드롭한다. `start`는 이 사유(`this daemon speaks protocol v<이전>; the hook speaks v<후보>`, `no daemon on … Connection refused`)를 실패가 아니라 `swap_window_drops`로 세고, `verdict`는 교체 후 60초 안의 것만 면제한다. 60초 뒤에도 같은 사유가 나오면 새 데몬이 거절하고 있다는 뜻이라 FAIL이다. 시작 복구의 gap fill이 일부를 되찾는다.

**계획된 재시작**: 1일 차와 6일 차에 `canary.sh restart`(`install --apply`를 돌리고 serving ms·재시작 중 `drops.jsonl`에 추가된 줄 수를 `~/.conbus/canary.json`에 기록 — `verdict`가 이 항목들을 계획된 것으로 제외한다. 세션 없는 페이로드 거절은 유실이 아니라 따로 표시).

**통과 조건** (`canary.sh verdict`가 교체 시각 이후 로그로 계산):

1. 계획 외 레코더 재시작 0.
2. 읽지 못한 줄 0.
3. 계획된 재시작에서 유실 0(추가된 드롭 줄 수), 새 드롭 사유 0 — 교체 후 60초 안의 예상 사유(`swap_window_drops`)는 면제되고 개수가 표시된다.
4. 7일 동안 활동이 있는 하네스(최근 실측 기준 `claude_code`, `codex`, `grok`, `cursor`) 전부 활동일마다 기록 있음. OpenCode는 S2로 대체.
5. twins·`repeated` 증가가 기준선 이하.
6. **닫힌 날마다** RC의 `usage --json` = 같은 로그를 읽은 0.2.8 (`tokens_*` 제외).
7. RSS와 M3 시간 예산 유지.
8. `policy`를 켜지 않았으니 결정 0건, `sync`에 참여 안 했으니 POST 0건.
9. 운영자 확인: 그 주에 에이전트 세션이 hook 때문에 깨진 체감이 없다. 도구로 잴 수 없는 유일한 항목이다.

**실패하면**: `canary.sh rollback`(`mv -f conbus.rollback conbus && conbus install --apply`) → 수정 → `rc.N+1` → 실패 단계부터. 레코더 경로를 고치지 않았으면 남은 일수만, 고쳤으면 최소 72시간.

**도구로 못 보는 것**: 유기적 quota hop은 두 달에 두 번이다. 7일 안에 오지 않을 수 있으며, 그래도 통과를 막지 않는다. 자동 hop을 experimental로 두는 이유다.

---

## 8. 1.0.0 자르기

canary 통과 + 운영자 승인 뒤:

1. PR: 버전 `1.0.0`, CHANGELOG 1.0.0 절, README·매뉴얼 버전. RC와의 diff는 이것뿐.
2. `v1.0.0` 태그 → draft 게시 → 서명·공증 → `gh release edit --draft=false --latest`.
3. 내려받아 확인, 이 Mac에 배포, 전략 페이지 갱신, HANDOFF 다음 단계.

일정(7일 canary를 11/2에 시작하고 재시작이 없을 때): **2026-11-10 전후**.

---

## 9. 운영자만 하는 일

| # | 결정·행동 | 시점 |
| --- | --- | --- |
| O1 | **공개 이력 재작성 여부.** `ca4474f`~`ac42cca`의 내용이 공개돼 있다(`docs/publication-review.md`). 재작성하면 모든 SHA·태그가 바뀌고, 옛 SHA는 GitHub 측 정리 전까지 열릴 수 있다. 할 거면 **Devin 실행 전**. RC와 1.0.0 사이에는 하지 않는다(RC 태그와 provenance가 고아가 된다). 1.0.0 뒤면 정식 태그의 SHA가 바뀐다. 그대로 두는 것도 선택지다 | 실행 전, 아니면 안 함 |
| O2 | canary 시작 한 줄 | RC 뒤 |
| O3 | `v1.0.0` 승인 | canary 뒤 |
| O4 | 랜딩·waitlist 배포(D1, `CONTACT_EMAIL`, DNS) | 원할 때 |
| O5 | 글 두 편, 외부 연락 | 1.0 뒤 권장 |
| O6 | RFD 0002·0003 수락, 호스팅·결제·법무 | 돈 게이트 뒤 |
| O7 | **어느 게이트가 살아 있는가.** V1(12회 인터뷰, 11/4, 집계표 공란) 또는 E1–E3 사다리. 사다리라면 E1 마감(11/4)을 1.0 일정에 맞춰 옮길 것을 권한다 | 11/4 전 |
| O8 | 흡수 확인 후 정리: `devin/*`·`iron-tesla` 작업 트리와 브랜치, `sync` 브랜치, `stash@{0}`(같은 내용이 커밋에 있음), 머지된 원격 브랜치. 공개 `main`의 `.claude/agents/quant-claim-auditor.md` 유지 여부 | 1.0 뒤 |
| O9 | 실제 Linux 호스트가 있으면 canary에 추가 | 선택 |

---

## 10. 1.x 뒤로 미룬 것

Copilot CLI 어댑터(설치·실 payload 필요), Gemini CLI 어댑터, 프롬프트·답변 저장 시 암호화, `MessageDisplay`, 남겨 둔 payload 필드(`model`, `permission_mode` 등), Homebrew, 해시 체인·서명 export(구매자가 요구할 때), AI 협업 보고서.

**여유 시간 백로그** (M7 + S가 일찍 끝났을 때만, 이 순서로): 잘랐던 항목을 역순으로 복구 → M3.8 `log compact` → 커버리지 측정(`cargo llvm-cov`)과 80%까지 테스트 보강 → 대형 파일(`daemon.rs` 2,283줄 등)은 **고치지 않고** 분할 계획만 기록 → S1·S3 재실행. 목록이 끝나면 멈춘다. 새 기능을 만들지 않는다.

---

## 11. 위험

| 위험 | 대응 |
| --- | --- |
| 수동 실행이 라이브 레코더를 내림 | `CONBUS_SKIP_LAUNCHCTL=1`·`CONBUS_HOME` 상시, 실행기가 환경에 강제, `canary.sh` 가드 |
| 훅 경로 변경이 벤더 세션을 조용히 깨뜨림 | 유효 JSON 불변식 테스트, S2·S3, canary 자동 롤백, 운영자 시작 |
| 사이드카가 다른 숫자를 냄 | 행 단위 동일 검증(M3.1, S1, canary 6번), `CONBUS_NO_CACHE`, 길이 대조 |
| 스풀에 프롬프트가 남음 | 같은 0700 루트, 보존 수준 적용, 상한, 수집 후 삭제, 레코더 없는 머신은 스풀 안 함 |
| 공개 저장소로 유출 | `check-public-safe.sh`, fixture 가림, 고정 프롬프트 |
| 한 번에 합치는 위험(RC 하나에 전부) | 이 계획이 받아들인 대가. RC 반복과 7일 canary로 상쇄 |
| 벤더 쿼터 소모 | 하네스당 세션 상한, 짧은 프롬프트 |
| 실행이 중간에 멈춤 | 상태 파일, 재진입, 실행기의 자동 재개 |
| 범위가 늘어남 | 자르는 순서, 여유 시간 백로그 외 금지 |

---

## 12. 계획 변경 규칙

단계를 나누거나 합치거나 건너뛰거나 순서를 바꾸면 아래 표에 **이유와 함께 한 줄** 적는다(마일스톤의 마지막 PR에 함께). 1절의 약속과 3절의 결정은 실행 중에 바꾸지 않는다. 바꿔야 할 것 같으면 보수적인 쪽으로 진행하고 운영자 목록에 올린다.

### 진행표

| 단계 | 상태 | PR | 측정값 | 메모 |
| --- | --- | --- | --- | --- |
| M0 | 완료 | #70 #71 #72 | `main` 테스트 707 통과·0 실패·1 무시(1.94와 1.99.1 같음) | 1.99.1 clippy가 계획의 2건 외에 4건(`needless_late_init` 1, `collapsible_match` 3)을 더 잡아 M0.2에서 함께 고침. M0.5: private vulnerability reporting·Dependabot alerts·secret scanning·push protection 켬, `main` 보호(필수 `gate`, strict 끔, 리뷰 없음, force push·삭제 금지). M0.6: v0.2.9 서명·공증 후 교체, 내려받아 `TeamIdentifier=4UN6WC576K`·`Notarized Developer ID` 확인, `latest`는 v0.2.9. 랜딩의 "공증 안 됨"·"실제 한도를 만난 적 없음" 문구도 사실과 달라 함께 고침 |
| M1 | 완료 | #76 #78 #79 #81 #82 | macOS 746 통과·0 실패·1 무시, Linux x86_64·aarch64 초록 | unlock의 기능 커밋 10개를 다섯 PR로 흡수. `git cherry`와 파일별 대조에서 남은 차이는 `site/`(M6)와 unlock 이후 `main`에 들어온 작업, 의도해 다르게 쓴 문서뿐. M1.2 의미 충돌(`sync.rs` 행 키)은 테스트로 고정. M1.3은 "정책 없음 = 바이트 동일", "못 읽는 정책 = 결정 없음 + doctor 경고"를 새 테스트로 고정. Linux CI가 잡은 결함 2건을 M1.4에서 함께 고침: macOS 전용 상수의 dead code, 스레드 테스트가 스텁을 쓰고 곧바로 실행할 때의 ETXTBSY(스텁을 자식 `sh`가 쓰게 함). `linux`·`linux-arm`을 `main` 필수 체크에 추가 |
| M2 | 완료 | #84 #87 #92 #100 | Claude Code 2.1.316, Codex 0.166.0, OpenCode 2.2.3, Grok 1.0.71, cursor-agent 2026.10.19, agy 1.4.12 실세션 캡처. `real_payloads` 17/17 | OpenCode 2.x가 0.2.x 플러그인을 처음부터 거부하고 있었다(벤더 로그에만 남음). 2.x 플러그인으로 고침(#84). Grok은 세션 종료 뒤 `Stop`을 한 번 더 보내고 `StopCancelled`가 등록돼 있지 않았다(#87). cursor-agent `-p`에는 프롬프트·턴 종료 훅이 없다(지원 표에 기록). 정책 deny는 Claude Code와 Codex 실세션에서 지켜졌다. 세션 사용: Claude 3, Codex 3, OpenCode 3, Grok 2, Cursor 1. Cursor IDE는 이 Mac에 없어 미확인 |
| M3 | 완료 | #73 #74 #83 #88 #91 #94 #95 #97 #99 #104 #105 | 캐시 첫 빌드 32.4 s·778 MB(로그 10.3 GB), 이후 `log --tail 1` 13 ms·`usage --days 7` 1.31 s, 캐시 유무 결과 행 단위 동일. 시작→서빙 11.0 s·RSS 445.5 MB. doctor 3.35 s(벤더 오프)·4.1 s(실제 PATH). 1,000-hook 스톰 무손실 | 목표 미달 2건을 측정으로 기록: 시작→서빙 11.0 s > 5 s(prepare의 메타 스캔이 5.3 s를 씀; 체크포인트는 설계만), doctor 3.35 s > 3 s(실제 PATH 병렬 프로브로 64.1→4.1 s). 스풀은 정확히 한 번을 약속하지 않고 중복 계산 의미를 지킨다. **Lead 대기**: M3.4 RSS(조사 완료 — `relock_derived`가 fold churn이 키운 빈 allocator 페이지까지 mlock하는 것이 주원인 후보, 개선은 Lead), M3.6 drops 표시(구현·테스트 완료 #116, 리뷰 대기) |
| M4 | 완료 | #85 #86 #89 #93 #101 #102 | 종료 코드·키 집합 골든 테스트 초록. 업그레이드 왕복 테스트 초록(macOS) | 안정 계약은 `docs/stability.md`(#85). 알 수 없는 명령·플래그·빠진 값은 exit 2, `hook`은 면제(#101). `usage --json`·`--shareable`·`progress --json`·`log --json` 키 집합 고정(#93). 레코드 봉투·이벤트 종류·프로토콜·롤백 규칙은 `docs/log-format.md`와 매뉴얼에(#86). RFD 0001 accept, 0002·0003 draft 유지(#89). 0.2.x 업그레이드는 설치 멱등·plist 환경 보존·uninstall이 Conbus 소유만 회수함을 테스트로 고정(#102). Linux systemd 왕복은 `#[cfg(target_os = "linux")]` 테스트로 추가(#115, 머지 대기 — Linux CI가 실행한다) |
| M5 | 완료 | #75 #80 #90 #96 #106 | sync reference 왕복 테스트 초록. M5.6 리뷰 CRITICAL·HIGH 0 | curl `-q` 첫 인자·TLS 고정·리다이렉트 거절·본문은 `--config -`와 stdin으로(#80). 추론 리스너 루프백 전용·읽기쓰기 타임아웃(#75). `-` 있는 태그는 prerelease, 안정 태그는 서명 전 draft(#90). reference 서버가 조각난 TCP 본문 결함 1건을 잡았고, TLS는 `CURL_CA_BUNDLE` 경로만 문서화(#106). M5.4는 구현됨(#117, 리뷰 대기): doctor가 관리형 파일을 읽어 `allowManagedHooksOnly` 아래 사용자 훅이 발화하지 않음을 말하고, `install --managed`가 root 외 쓰기 가능 바이너리를 경고 |
| M6 | 완료 | #98 #103 | `check-docs.sh` 초록(0.2.9, 명령 19개). site 테스트 6/6(vitest) | `check-docs.sh`가 첫 실행에서 실제 누락 4건을 잡았다(README·한국어 매뉴얼에 `policy`, 두 매뉴얼에 `sync`). 한국어 매뉴얼은 Linux 미지원이라고 적혀 있던 것을 고치고 정책·프로필·sync 절을 넣었다. 랜딩은 `site/` 하나로 합치고 `docs/www/`를 지웠다. site는 `site/**` 경로 필터 CI로만 돌고 배포하지 않았다 |
| M7 | 완료 | #108 #109 | `v1.0.0-rc.1` 태그 게시, `pre=true`·`draft=false`, `latest`는 v0.2.9 유지. 릴리스 드라이런 3타깃 초록 | canary.sh·shadow-replay.sh·canary-rehearsal.sh(#108). RC PR #109: 버전 범프·Cargo.lock·CHANGELOG·매뉴얼 헤드라인 |
| S1–S5 | 완료 | #110 | S1: 0.2.8↔RC 읽기 차이는 `tokens_*` 키·새 doctor 항목뿐, 캐시 유무 동일; RC가 969,129레코드 전수 읽고 0.2.8이 RC 기록을 역방향으로 읽음. S3 스톰 10/10 무손실 + 알 수 없는 스키마 스풀 유지(레코더 동작 중). S4 Colima ARM64 설치→유닛→훅→doctor→재시작→uninstall 전 단계 통과(첫 보고는 증거 누락으로 재실행). S5 TLS 왕복 | S2 실세션 패리티(claude·codex 각 4==4; grok 프로젝트 훅 없음·cursor 미인증·opencode 비대화형 이벤트 없음 — 한계로 기록). OpenCode 2.2.5 실세션은 별도 검증에서 `turn-started`·`turn-ended Complete` 기록 확인(성공 경로는 어댑터 수정 #114 필요). S5가 refserver의 와이어 불일치 3건(`day_closed`, `accepted` 형상, 장치 id 형식)을 잡아 시험 하니스를 고쳤다(#110). Linux 실벤더는 미검증 |
| Canary | rc.2 뒤 시작 | #110 #113 | `canary.sh`를 §7 계약대로 재작성: start가 preflight를 먼저 돌리고, 교체 뒤 serving·build·훅 왕복·드롭 검사 실패 시 자동 롤백. `restart` 서브커맨드와 `canary.json` 창 기록 추가. verdict가 재시작 분류·패리티·RSS·시간 예산을 실제로 계산. 리허설이 CI gate에서 돈다 | 라이브 교체는 하지 않음. rc.1에서 발견된 결함(OpenCode `turn-ended` #114 등)이 있어 canary는 그 수정이 들어간 `rc.2`로 시작한다. rc.1 바이너리 자체는 Developer ID 서명·공증 완료 |
| 1.0.0 | | | | |

### 변경 기록

| 시각 | 변경 | 이유 |
| --- | --- | --- |
| 2026-11-01 09:20 | M5.3(`--inference-listen` 루프백 전용)과 M5.1(curl `-q` 등)을 M1과 병렬로 먼저 머지(#75, #80) | 보안 항목이고 M1–M4와 파일이 겹치지 않아 Lead가 기다리는 시간에 처리 |
| 2026-11-01 09:25 | 계획에 없던 테스트 수정 PR #77 추가 | `real_payloads` Codex 테스트가 소켓 bind만 보고 데몬을 기다려 느린 러너에서 실패(문서만 바꾼 #76이 두 번 연속 실패). 구조적 수정은 M3.2 |
| 2026-11-01 09:50 | M2의 F1(OpenCode 2.x)·F2(Grok)·F4(지원 표)를 Lead가 처리(#84 #87 #92) | Sidekick이 M3 임계 경로에 있는 동안 병렬로 처리 |
| 2026-11-01 11:45 | Linux systemd 업그레이드 왕복 테스트 제외 | 절단 순서표의 선택 항목; macOS 경로만 검증(#102) |
| 2026-11-01 11:45 | 시작→서빙 목표 5 s를 11.0 s 측정 기록으로 대체, 체크포인트는 설계만 | prepare의 메타 스캔이 5.3 s를 써 남은 예산으로 목표 불가; 계획은 측정·설계 기록을 허용 |
| 2026-11-01 11:45 | M3.4 RSS 개선·M3.6 drops 이유 표시 제외 | 절단 순서표의 선택 항목; 필수 항목이 먼저 |
| 2026-11-01 11:50 | M5.4 관리형 설치(doctor 읽기·`install --managed` 경고) 제외 | 절단 순서표 #4; S5에 필요한 reference 서버(M5.2)가 먼저 |
| 2026-11-01 13:xx | 잘렸던 항목 복구: M4.5 Linux systemd 왕복 테스트(#115), M3.6 drops 사유 표시(#116), M5.4 관리형(#117) — 머지·리뷰는 Lead | 앞선 세션이 예산을 남기고 절단한 항목들; SWE-2 세션이 되살림 |
| 2026-11-01 13:xx | `usage_otlp` 닫힌 포트 테스트의 비결정성 수정 머지(#111); `src` 테스트 모듈의 동일 패턴은 별도 PR(#112, 열림) | main CI gate가 간헐 실패로 빨갰다 — connect가 "refused"가 아니라 "reset"을 받는 포트 경쟁 |
| 2026-11-01 13:xx | `canary.sh`를 §7 계약으로 재작성 머지(#113): 자동 롤백·계획 재시작·창 고정 verdict·패리티·RSS·시간 예산·blackout 문법; 리허설을 macOS CI gate에 | 이전 구현은 안내 문구만 출력하고 보고서가 말한 검사를 하지 않았다 |
| 2026-11-01 13:xx | OpenCode 2.x 어댑터: `session.execution.succeeded` → `turn-ended Complete` 전달 추가(#114, 열림) | 실세션 검증에서 `turn-started`만 남고 완료 턴은 drops(`no mapping`)로 버려짐을 확인 |
| 2026-11-01 13:xx | S4 증거 재수집·S3 미지원 스키마 재검증·S6 `install --apply` 차이 측정(Codex 재신뢰 불필요 확인)·벤치 재측정·M3.4 RSS 조사·커버리지 측정 | 첫 보고의 S4 증거는 209바이트 fatal 한 줄이었고 여러 경로가 미검증이었다 |
