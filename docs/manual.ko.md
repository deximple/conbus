# conbus 매뉴얼

이 매뉴얼은 macOS와 Linux용 **conbus 1.0.0-rc.3**를 설명합니다. 짧게 훑으려면
[README](../README.md)를, 릴리스 사이에 무엇이 바뀌었는지는
[CHANGELOG](../CHANGELOG.md)를 보세요. English: [manual.md](manual.md)

- [1. conbus란](#1-conbus란)
- [2. 설치하기 전에](#2-설치하기-전에)
- [3. 설치](#3-설치)
- [4. 작동 확인](#4-작동-확인)
- [5. 기록](#5-기록)
- [6. 브리핑](#6-브리핑)
- [7. hop](#7-hop)
- [8. 쿼터](#8-쿼터)
- [9. 지난 기록 되살리기](#9-지난-기록-되살리기)
- [10. 체크아웃끼리 공유하는 사실](#10-체크아웃끼리-공유하는-사실)
- [11. 내 데이터](#11-내-데이터)
- [12. 업그레이드](#12-업그레이드)
- [13. 제거](#13-제거)
- [14. 문제 해결](#14-문제-해결)
- [15. 로컬 정책](#15-로컬-정책)
- [16. Claude Code 프로필: 손으로 바꾸는 두 번째 계정 (experimental)](#16-claude-code-프로필-손으로-바꾸는-두-번째-계정-experimental)
- [17. 레퍼런스](#17-레퍼런스)
- [18. 알려진 한계](#18-알려진-한계)

---

## 1. conbus란

코딩 에이전트를 둘 이상 쓰고 계실 겁니다. 에이전트마다 자기 기록이 있고,
지금 무슨 일을 하던 중인지에 대한 자기만의 이해가 있고, 자기 쿼터가 있습니다.
그중 하나가 작업 도중에 멈추면 — 5시간 한도든 주간 상한이든 — 다음 에이전트는
아무것도 모른 채 처음부터 시작합니다.

conbus는 이미 쓰고 계신 에이전트들 아래에 자리 잡습니다. 모든 프롬프트, 답변,
툴 호출이 **로컬 로그 하나**에 쌓이고, 에이전트가 바뀌어도 그대로 유지되는 대화
id 아래에 정리됩니다. conbus는 그 로그를 바탕으로 같은 체크아웃(프로젝트 폴더)에서
새 세션을 열면 최근 세션들이 무엇을 하던 중이었는지 **브리핑**하고, 대화를 다른
에이전트로 **hop**할 수 있습니다. 벤더가 쿼터 때문에 턴을 멈추면 스스로, 또는
여러분이 시키면 그때 옮깁니다.

화면은 여전히 각 에이전트의 터미널과 앱입니다. conbus에는 채팅 UI가 없습니다.

### 이 매뉴얼에서 쓰는 말

| 말 | 뜻 |
| --- | --- |
| **하네스** 또는 **레그** | 에이전트 하나: `claude_code`, `codex`, `cursor`, `grok`, `opencode`, `agy` |
| **대화** | 작업 하나. 여러 레그에 걸칠 수 있고, **버스 id**(UUID)로 구분합니다 |
| **레코더** | `conbus daemon`. 모든 훅을 받아 로그에 덧붙입니다 |
| **훅** | 각 벤더의 훅 설정에 등록된 `conbus hook`. 벤더가 이벤트마다 실행합니다 |
| **브리핑** | conbus가 벤더 자신의 채널로 새 세션에 건네는 맥락 |
| **hop** | 대화를 다른 레그로 옮기는 것. 쿼터 중단이 강제했으면 *displaced*(밀려남), 여러분이 요청했으면 *handed off*(넘겨줌) |
| **웜 세션** | conbus가 다음 레그에 직접 열어 대기시켜 두는 세션. hop은 여기에 도착합니다 |
| **테이크오버** | hop이 `conbus run`에 남기는 쪽지. 같은 터미널에서 다음에 어떤 레그를 띄울지 알려 줍니다 |

---

## 2. 설치하기 전에

- **macOS**(Apple silicon 또는 Intel) 또는 **Linux**(x86_64, aarch64). 레코더는
  Mac에서는 launchd 에이전트, Linux에서는 systemd 사용자 유닛으로 돕니다. Linux용은
  정적 musl 바이너리(`conbus-linux-x86_64.tar.gz`, `conbus-linux-aarch64.tar.gz`)이고,
  `install.sh`가 `uname -m`으로 고릅니다. Windows는 지원하지 않습니다.
- **에이전트 하나 이상, 자기 바이너리로 이미 로그인된 상태**: Claude Code(`claude`),
  Codex(`codex` 또는 Codex.app), Cursor, Grok Build CLI(`grok`), opencode 중 하나.
  conbus는 대신 로그인하지 않고, 그 자격 증명을 읽지도 않습니다.
- **`PATH`에 `~/.local/bin`**이 들어 있거나, 추가할 준비. 훅은 바이너리를 절대 경로로
  부르므로 어느 쪽이든 동작하지만, 셸에서 `conbus`를 찾으려면 필요합니다.

저장소가 비공개인 동안에는 릴리스에서 설치할 때 접근 권한이 있는 계정으로 GitHub
CLI에 로그인해 두어야 합니다: `gh auth login`.

---

## 3. 설치

### 릴리스에서 설치

```bash
curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
```

이 스크립트는:

1. 최신 릴리스에서 `conbus-macos-universal.tar.gz`를 받습니다. 익명 다운로드가
   거부되면 `gh release download`로 대신 받습니다.
2. 공개된 SHA-256과 대조하고, 다르면 아무것도 설치하지 않고 멈춥니다.
3. 바이너리를 `~/.local/bin/conbus`에 놓습니다. 옆에 쓴 다음 이름을 바꾸므로,
   훅을 실행하던 에이전트가 도중에 끊기지 않습니다.
4. `conbus install`을 실행합니다. 이 명령은 무엇을 등록할지 **보고만** 합니다.

이 시점까지 에이전트 설정은 하나도 바뀌지 않았습니다. 옵션:

| 변수 | 효과 |
| --- | --- |
| `CONBUS_VERSION=v0.2.8` | 최신 대신 그 릴리스를 설치 |
| `CONBUS_REPO=owner/name` | 포크에서 설치 |
| `CONBUS_BASE_URL=…` | 미러에서 받기. 로컬 빌드는 `file:///path/to/dist` |

### 소스에서 빌드

Rust 1.99 이상:

```bash
git clone https://github.com/deximple/conbus
cd conbus
cargo build --release -p conbus
./target/release/conbus install
```

### 훅 등록

```bash
conbus install --apply
```

**이 Mac에 설치된 에이전트마다** — 에이전트 자신의 설정 디렉터리로 알아봅니다 —
그 에이전트 설정에 훅을 하나씩 병합합니다. 설치되지 않은 에이전트는 건너뜁니다.
나중에 설치했다면 `install --apply`를 다시 실행하세요.

| 에이전트 | 파일 | 등록되는 것 |
| --- | --- | --- |
| Claude Code | `~/.claude/settings.json` | 이벤트 13개, 그리고 StatusLine(감싸서 등록하므로 원래 쓰던 상태 표시줄도 그대로 보입니다) |
| Codex | `$CODEX_HOME/hooks.json` (기본 `~/.codex`) | 이벤트 7개 |
| Cursor | `~/.cursor/hooks.json` | `sessionStart`, `beforeSubmitPrompt`, `stop`, `sessionEnd`, `afterAgentResponse` |
| grok | `~/.grok/hooks/conbus.json` | 이벤트 10개 |
| opencode | `~/.config/opencode/plugins/conbus.js` | 세션·메시지 이벤트를 전달하는 플러그인 |

바이너리가 아직 없으면 `~/.local/bin/conbus`에 놓고, launchd 에이전트
`~/Library/LaunchAgents/sh.conbus.daemon.plist`를 씁니다. 이 에이전트가 레코더를
계속 돌리고 로그인할 때 띄웁니다.

바꾸는 파일마다 옆에 사본(`…json.conbus-backup`)을 남깁니다. 원래 있던 훅과 설정은
순서까지 그대로 두고, 올바른 JSON이 아닌 파일은 보고만 하고 건드리지 않습니다.

### Codex에서 훅 승인

Codex는 새 훅을 승인받은 뒤에만 실행합니다. Codex를 한 번 켜면 새로 생겼거나
바뀐 훅을 보여 주고 신뢰할지 묻습니다. 승인하기 전까지 Codex 훅은 등록만 된 채
아무 일도 하지 않습니다. conbus는 그 승인을 대신 쓰지 않습니다. 동의는 conbus가
아니라 여러분이 하는 것입니다.

### 관리되는 Mac에서

조직이 Claude Code를 관리하고 그 정책이 `allowManagedHooksOnly`를 켜 두었다면,
`install --apply`가 등록한 훅은 실행되지 않습니다. 이때 `conbus install --managed`는
관리자가 Claude Code 관리 설정(managed settings)에 대신 넣을 훅 블록을 출력하고,
어디에 넣는지 알려 줍니다. 아무것도 쓰지 않습니다. 나머지 — 바이너리 배포, 각자
`install --apply`로 띄우는 레코더, 로그인 항목 승인, 모두 다시 빼기 — 는
[Managed Macs](fleet.md)(영문)를 보세요.

---

## 4. 작동 확인

1. 프로젝트 폴더에서 아무 에이전트나 열고 프롬프트를 하나 보냅니다.
2. `conbus log --tail 5`를 실행합니다. 방금 보낸 턴이 있습니다.
3. `conbus doctor`를 실행합니다.

`doctor`는 사실 하나를 한 줄로 보여 줍니다. 먼저 봐야 할 줄은 이렇습니다.

| 줄 | 정상 |
| --- | --- |
| `build` | `0.2.8 (<commit>)` |
| `socket` | `listening` |
| `daemon` | `build`와 같은 커밋. `DIFFERS`는 바이너리를 바꾸고 데몬을 다시 시작하지 않았다는 뜻입니다 |
| `writable` | `appendable` |
| `hooks` / `events` | 설치된 에이전트마다 `runs`와 `N/N registered`. 나머지는 `not installed here` |
| `codex` | `7 hook(s) trusted`. `NOT TRUSTED`는 아직 Codex에서 승인하지 않았다는 뜻입니다 |
| `last` | 방금 보낸 턴 |
| `drops` | 작업하는 동안 늘지 않음 |

나머지 줄:

| 줄 | 알려 주는 것 |
| --- | --- |
| `hardening` | 레코더가 코어 덤프를 끄고 메모리를 잠갔는지 |
| `chat` | Claude.ai 대화가 로그에 들어오는 경로(9장) |
| `log` | 레코드 수, 대화 수, 레코더가 읽지 못한 줄 수(0이어야 합니다) |
| `twins` | 실시간 기록과 백필 양쪽에 기록된 세션, 그리고 어느 사본으로 이어지는지 |
| `nameless` | 예전 빌드가 빈 세션 id 아래 묶어 둔 대화(14장) |
| `briefing` | 브리핑을 받은 새 세션 수와, 나머지가 받지 못한 이유(6장) |
| `quota` | 각 레그에 남은 양(8장) |
| `hops` / `excluded` | 스스로 일어난 hop, 그리고 직접 입력했거나 테스트로 연출한 hop(7장) |
| `inject` | 브리핑과 hop이 더한 맥락의 양. 비용으로 봅니다 |
| `turns` | 사람이 쓴 턴, 프로그램이 쓴 턴, 표시가 없는 턴 |
| `route` | 지금 hop하면 갈 곳 |
| `takeover` | `conbus run`을 기다리는 hop이 있는지, 따르기엔 너무 오래됐는지 |
| `warm` | conbus가 다른 레그에 열어 두고 붙잡고 있는 세션 |

`conbus hop --demo`는 가짜 failover를 기록합니다. 에이전트 창은 열리지 않습니다.
실제 hop이 일어나기 전에 로그에서 hop이 어떻게 보이는지 확인할 수 있습니다.

---

## 5. 기록

따로 할 일은 없습니다. 훅이 등록되면 레코더가 백그라운드에서 돌고, 설치된 모든
에이전트의 모든 이벤트가 로그에 쌓입니다.

**기록되는 것**(기본값): 프롬프트와 답변, 어떤 툴을 호출했고 성공했는지, 얼마나
걸렸는지, 권한 요청, 쿼터·오류로 인한 중단, 세션 시작과 끝, 서브에이전트 턴.
**툴의 입력과 출력은 쓰지 않습니다.** 크기만 남깁니다. 바꾸려면 11장을 보세요.

**다시 읽기:**

```bash
conbus log --tail 20                  # 마지막 20개 레코드
conbus log --bus-id 01a0c7f2          # 대화 하나, 거쳐 간 모든 레그 (앞부분만 써도 됩니다)
conbus log --since 2026-09-20         # 날짜, 또는 타임스탬프의 앞부분
conbus log --grep "migration"         # 이 문자열이 들어간 레코드
conbus log --json                     # 한 줄에 JSON 레코드 하나, 스크립트용
conbus log --reveal --bus-id 01a0c7f2 # 봉인된 툴 본문 복호화 (11장)
```

**세어 보기:** `conbus usage`는 로그를 에이전트별로 무엇을 했는지로 접어 보여
줍니다. 기본은 최근 7일이고, `--days 30`, `--since 2026-09-01`, 또는 끝난 하루
`--day 2026-09-22`로 바꿉니다. 아래는 이 도구를 만든 Mac의 30일입니다.

```text
usage     from 2026-08-25 (UTC) · 131666 record(s), 4397 recovered · not counted: earlier 33138 · repeated 10610 · recorded twice 24
harness   claude_code — 2657 conversation(s): attended 240 · unattended 2354 · unknown 63
          3207 turn(s) · subagents 1016
          stopped: limit 211 in 3 episode(s) · account 111 · vendor 3 · request 0 · other 1 · by a person 0
          27982 tool call(s): failed 545 · interrupted 0 · refused 48 · asks 247 · 21h in tools
harness   codex — 183 conversation(s): attended 70 · unattended 0 · unknown 113
          1419 turn(s) · subagents 0
          stopped: limit 0 in 0 episode(s) · account 0 · vendor 0 · request 0 · other 0 · by a person 0
          1892 tool call(s): failed 0 · interrupted 0 · refused 0 · asks 1 · 0s in tools
failing   Bash 453 · Read 61 · Glob 4
busiest   /Users/you/project 2151 · /Users/you/other 145 (turns)
cost      not in this log: no hook payload carries tokens or prices
```

- **attended · unattended · unknown**: 그 세션에 사람이 있었는지입니다. 세션이
  어떤 프런트엔드에서 돌았는지로 판단하는데, 이를 알려 주는 건 Claude Code뿐입니다.
  Claude Code 세션 안에서 띄운 다른 에이전트는 그 값을 물려받고, 나머지는
  `unknown`입니다.
- **stopped**: 턴이 일찍 끝난 이유를, 사용자가 무엇을 해야 하는지로 나눕니다.
  **limit** — 사용 한도(한 에이전트에서 1시간 안쪽으로 이어진 중단은 **에피소드**
  하나), **account** — 인증·결제·계정 정지, **vendor** — 벤더 과부하·장애,
  **request** — 잘못된 요청·너무 긴 출력·없는 모델, **other** — 이 버전이 모르는
  사유, **by a person** — 사용자가 멈춤.
- **subagents**는 끝난 서브에이전트 실행 수입니다. 턴이 아니며, 서브에이전트가
  어떻게 끝났는지는 중단으로 세지 않습니다.
- **failed · interrupted · refused · asks**: 실행됐다가 실패한 툴 호출, 사용자가
  멈춘 호출, 에이전트가 실행을 거부한 호출, 권한을 물어본 횟수입니다.
  **in tools**는 에이전트가 알려 준 실행 시간의 합이고, Codex는 알려 주지 않습니다.
- **recovered**: 기록기가 그때 보지 못하고 나중에 복구한 기록입니다. 기록기가
  꺼져 있던 동안 빠진 턴(갭 채우기)과, 훅이 없는 Codex.app 스레드처럼
  `backfill`로 들여온 대화가 여기에 들어갑니다. 일어난 시각으로 세므로 기간보다
  오래된 과거는 나오지 않습니다.
- **not counted**: 기간 앞뒤의 기록, **repeated**(같은 보고가 두 번 전달된 것 —
  에이전트에 따라 이벤트 하나가 로그에 두 번 들어옵니다), **recorded twice**(훅도
  기록한 세션의 복구 사본), **probe**(conbus 시험이라고 스스로 밝힌 실행)입니다.
- 한 턴에 중단을 여러 번 보고한 Cursor 턴은 가장 심각한 하나로 셉니다. Cursor는
  턴의 끝을 여러 훅에서 씁니다. 비용은 세지 않습니다. 토큰이나 가격을 싣는 훅이
  없습니다.

`conbus usage --json`은 날짜·에이전트·폴더·사람 여부마다 한 줄씩 출력합니다.
숫자만 있고, 프롬프트·답변·툴 본문은 절대 들어가지 않습니다. `conbus usage
--shareable`은 같은 줄에서 폴더를 뺀 것 — 동기화가 생긴다면 보낼 내용
([RFD 0002](rfd/0002-metadata-sync.md))입니다. `--shareable` 자체는 아무것도
보내지 않습니다. 보내기 전에 먼저 읽어 볼 수 있게 하려는 것입니다.

**폴더별로:** `conbus progress`는 같은 숫자를 작업한 폴더별로 다시 묶습니다.
폴더마다 거기서 일한 에이전트, 턴 수(그중 사람이 있던 턴), 툴 호출과 실패, 한
에이전트의 일을 다른 에이전트로 넘긴 hop과 브리핑, 그리고 기간 동안 그 폴더의
`HEAD`에 쌓인 커밋 수를 보여 줍니다. `git`에는 호출마다 5초, 보고서 전체에 15초
제한을 두고 묻습니다. 체크아웃과 그 작업 트리는 본 체크아웃 아래 한 곳으로
묶이고, 이미 지운 작업 트리도 그것이 있던 폴더로 자리를 찾습니다. `git`이 어디에도
넣지 못하는 폴더는 따로 나오고, 커밋 수는 0이 아니라 빈칸으로 둡니다. 사람이 있던 턴이 많은 순으로 여덟 곳을 출력합니다.
`--workspace DIR`은 그 폴더와 그 아래, 저장소라면 어디에 있든 그 작업 트리까지 남기고, `--json`은 모든 폴더를 한 줄에
하나씩 출력합니다. `usage`와 마찬가지로 숫자만 있고 본문은 없습니다.

```bash
conbus progress --workspace .     # 이 체크아웃, 최근 7일
conbus progress --days 30 --json  # 모든 폴더, 한 달
```

**내가 쓰는 모니터링으로:** `--otlp URL`은 같은 줄을 OpenTelemetry 메트릭으로 이
Mac의 컬렉터에 보냅니다. OpenTelemetry Collector, Grafana Alloy, Datadog Agent는
모두 `http://127.0.0.1:4318`에서 받고, 설정된 곳(Grafana, Datadog, CloudWatch)으로
넘깁니다. 숫자 하나하나가 메트릭이 되고(`conbus.turns`, `kind`별 `conbus.stops`,
`conbus.tool.failures` 등 — 전체 목록은
[RFD 0002](rfd/0002-metadata-sync.md#the-pipe-the-same-rows-into-a-collector-the-person-runs)),
에이전트와 사람 여부가 붙습니다. 이 플래그가 없으면 아무것도 보내지 않습니다.

```bash
conbus usage --day 2026-09-22 --shareable --otlp http://127.0.0.1:4318   # 끝난 하루의 줄을 출력하고 보냄
```

보내는 단위는 `--day`입니다. 끝난 어제를 한 번만 보내세요. 각 값은 그날 일어난
일이라서 같은 날을 두 번 보내면 두 번 세고, 오늘은 아직 늘어나는 중입니다. 오늘을
보내면 conbus가 그렇다고 알려 줍니다. UTC 자정 직후에 보내는 것이 좋습니다. 몇 시간
전 시각이 찍힌 값을 버리는 백엔드가 있기 때문입니다(Datadog은 Historical Metrics
Ingestion을 켜지 않으면 1시간보다 오래된 값을 받지 않습니다). 이 첫 버전은 이 Mac의
컬렉터에만 평문 HTTP로 보냅니다. `https://`와 다른 호스트는 거부하고, 그 너머의 TLS는
컬렉터가 맡습니다. 다시 시도하지도 않습니다. 거부된 URL, 연결 실패, 2xx가 아닌 응답,
컬렉터가 거부했다고 밝힌 값은 오류이고, 명령은 0이 아닌 코드로 끝납니다.

레코드 하나는 이렇게 생겼습니다(필드는 이벤트마다 다릅니다).

```json
{"v":3,"ts":"2026-09-22T19:23:42.504Z","seq":144032,"bus_id":"01a0c7f2-4b1e-7d20-9c3a-5e8f1a2b3c4d",
 "harness":"claude_code","vendor_session_id":"4e2f…","workspace":"/Users/you/project",
 "entrypoint":"cli","event":"tool_requested","tool":"Bash","tool_use_id":"toolu_…",
 "input":{"kind":"elided","bytes":464}}
```

**프로그램과 사람을 구분합니다.** SDK, MCP 클라이언트, GitHub Action이 시작한
세션은 무인(unattended)으로 표시하고, 벤더가 스스로 기계가 썼다고 표시한
프롬프트도 따로 표시합니다. 다른 세션처럼 기록되지만 브리핑도 hop도 하지 않습니다.

**레코더가 꺼져 있어도** 훅은 올바른 형식의 답을 곧바로 돌려주므로 에이전트가
막히지 않습니다. 그 이벤트는 버려지고 `~/.conbus/drops.jsonl`에 남으며, `doctor`가
셉니다. 레코더가 다시 켜지면 꺼져 있던 동안 놓친 턴을 에이전트 자신의
트랜스크립트에서 채워 넣습니다.

---

## 6. 브리핑

폴더에서 새 세션을 시작하면 conbus는 같은 폴더의 최근 대화를 — 에이전트를 가리지
않고 — 살펴보고, 그 대화들이 무엇을 하던 중이었는지 짧게 정리해 새 세션에 건넵니다.
벤더 자신의 맥락 채널을 씁니다(Claude Code와 Codex는 `additionalContext`, Cursor는
`sessionStart`의 `additional_context`). 대화는 최대 **3개**까지 다루고 **약 2,600자**를
넘지 않습니다. 넘치면 글을 중간에서 자르지 않고 대화를 통째로 뺍니다.

다음 경우에는 브리핑하지 **않습니다**.

| `doctor` 표시 | 이유 |
| --- | --- |
| `silent/nothing_to_say` | 그 폴더에 충분히 최근이거나 충분히 긴 대화가 없음 |
| `silent/arrived_with_context` | 세션이 처음부터 긴 자기 설명을 들고 시작함 |
| `silent/unattended` | 프로그램이 돌리는 세션 |
| `silent/machine_authored` | 프로그램이 썼다고 프롬프트 스스로 밝힘 |
| `silent/no_workspace` | 벤더가 어느 폴더인지 알려 주지 않음 |
| `silent/workspace_too_broad` | 폴더가 `/`나 홈 디렉터리 |

브리핑을 완전히 끄려면 에이전트가 도는 환경에 `CONBUS_BRIEFING=off`를 설정하세요.
기록은 계속됩니다.

브리핑은 내 로그의 글을 다른 세션에 넣는 것입니다. 그 세션의 벤더는 여러분이 입력한
다른 글과 똑같이 그것을 받습니다(11장).

---

## 7. hop

hop은 지금 대화를 다른 레그로 옮깁니다. conbus는 그 결정을 로그에 기록하고,
목적지에 **웜 세션**을 열거나 새로 고친 뒤, 여러분을 그리로 데려갑니다.

### conbus가 스스로 hop하는 경우

| 신호 | hop |
| --- | --- |
| 쿼터 때문에 턴이 멈춤(`rate_limit`, `StopFailure`, 벤더의 쿼터 코드) | Claude Code ↔ Codex. 다른 레그는 Codex로 넘침 |
| 계정이 한도 도달을 보고함 | 위와 같음 |
| 80% 이상 사용했고 리셋까지 2시간 넘게 남음 | 벽에 닿기 전에 미리 넘겨줌 |
| 정책이나 인증 때문에 거부됨 | **hop 안 함** — 쿼터 문제가 아닙니다 |
| 목적지도 소진됨 | 대신 **기다림** |
| 무인 세션(프로그램) | **hop 안 함** |

0.2.8에서 자동 hop은 실험적 기능입니다. 실제 쿼터 중단에서 한 번(2026-09-27)
발동했지만, 다음 레그는 결정 21.6분 뒤, 한도가 이미 풀린 뒤에 준비됐습니다.
이제 `daemon.log`에 자동 hop마다 무엇을 결정했고 얼마나 걸렸는지 남습니다.

### 직접 hop하기

```bash
conbus hop                # 미리보기: 지금 대화는 어디로 갈까?
conbus hop cursor         # Cursor로 가는 hop 미리보기
conbus go cursor          # 실행
conbus hop --from 01a0c7f2 --to codex --apply   # 특정 대화
```

"지금 대화"는 이 Mac에서 가장 최근 턴이 있었던 대화입니다. 다른 대화를 고르려면
`--from`에 버스 id(또는 그 앞부분)를 넘기세요.

에이전트 안에서 말로 해도 됩니다. 프롬프트 전체가 **오직** 지시일 때
`conbus go`로 처리합니다.

- `switch to cursor`, `hop to codex`, `hand off to grok`, `go to opencode`,
  `transfer to claude`, `continue on codex`, `resume cursor`
- `이제 커서로 작업할거야`, `코덱스로 넘겨`, `클로드로 재개`

다른 일을 부탁하면서 에이전트 이름을 언급하기만 한 문장은 지시가 아닙니다.

### hop이 도착하는 곳

- **Claude Code와 Codex**: conbus가 직접 연 웜 세션. 여러분이 연 스레드에는 절대
  들어가지 않습니다. Codex.app은 그 스레드로(`codex://threads/…`), Claude 데스크톱
  앱은 그 세션으로(`claude://code/continue?session=…`) 열리고, `conbus run`
  터미널에서는 그 자리에서 이어집니다.
- **Cursor**: `cursor -n <folder>`가 새 Cursor 창에 프로젝트를 엽니다.
- **grok, opencode, agy**: `conbus run`이 기다리는 터미널로 갑니다. 터미널 없이
  일어난 hop — 쿼터 중단 때 레코더가 직접 한 hop — 은 대신 새 Terminal 창을 엽니다.

### `conbus run`: 터미널 하나, 에이전트 여럿

```bash
conbus run claude_code
```

지금 터미널에서 에이전트를 띄우고 그 부모 프로세스로 남습니다. hop이 일어나면
`conbus run`이 지금 에이전트를 멈추고 다음 에이전트를 **같은 터미널에서** 띄워 웜
세션을 이어 갑니다. 레그: `claude_code`(기본값), `codex`, `cursor`, `agy`,
`opencode`, `grok`.

어떤 `conbus run`도 **10분** 안에 가져가지 않은 hop은 나중에 따르지 않고 버립니다.
`conbus run`은 시작할 때 그렇다고 알려 줍니다. hop을 가져갈 때는 요청한 레그 대신
어떤 레그를 띄우는지 말해 줍니다.

### 기록 보기

`conbus status`는 라우팅 규칙, 레그별 쿼터, 최근 hop을 보여 줍니다. 직접 입력한
hop과 테스트로 연출한 hop은 스스로 일어난 hop과 따로 셉니다(`doctor`의
`excluded`).

---

## 8. 쿼터

```bash
conbus quota
```

| 레그 | 숫자의 출처 |
| --- | --- |
| Codex | Codex 자신의 app-server(`account/rateLimits/read`). Codex.app을 포함한 계정 전체 |
| agy | `agy -p /usage`. `agy models`로 로그인을 확인한 뒤에만 실행 |
| Claude Code | Claude Code가 StatusLine 명령에 넘겨주는 `rate_limits` |
| Cursor, grok, opencode | 아직 없음. 이들의 훅에는 남은 쿼터 필드가 없습니다 |

**Claude의 숫자는 터미널 `claude`에서만 보입니다.** 데스크톱 앱과 SDK 세션은
StatusLine을 실행하지 않으므로 판독값이 며칠 묵을 수 있습니다. 90분이 지나면
보여 주기만 하고 라우팅에는 쓰지 않으며, `quota`가 그 판독이 얼마나 오래됐는지와
StatusLine이 마지막으로 언제 돌았는지 알려 줍니다. conbus는 다른 방법으로 그
숫자를 얻으려고 Claude의 자격 증명을 읽지 않습니다.

**agy는 로그인돼 있을 때만 읽습니다.** 로그인하지 않은 `agy -p`는 브라우저 탭을
열고, 시간 제한으로는 이를 막지 못합니다. 그래서 conbus는 먼저 `agy models`를
실행합니다. 로그인이 없다고 나오면 `quota`, `doctor`, `status`가
`AUTH_REQUIRED`라고 알리고, 다른 이유로 실패하거나 시간이 다 되면 `SKIP`이라고
알립니다. 어느 쪽이든 `agy -p`는 실행되지 않고 판독값도 없으며, hop은 판독값 없이
진행합니다. 로그인하려면 터미널에서 `agy`를 실행하세요.

---

## 9. 지난 기록 되살리기

레코더는 설치된 뒤에 일어난 일만 봅니다. 그 전의 기록은 에이전트 자신의 기록에서
되살릴 수 있습니다.

```bash
conbus backfill            # 무엇을 되살릴지 보고
conbus backfill --apply    # 기록
```

Claude Code의 트랜스크립트(`~/.claude/projects`)와 Codex의 스레드(Codex 자신의
app-server로)를 읽어, 세션마다 "되살린 것"으로 표시한 대화로 정리합니다. 레코더가
이미 실시간으로 본 세션은 중복해서 만들지 않습니다.

`--apply`는 레코더가 도는 동안에는 거부합니다. 둘이 같은 파일에 쓰게 되기
때문입니다. 레코더를 멈추고, 백필하고, 다시 켜세요.

```bash
launchctl bootout gui/$(id -u)/sh.conbus.daemon
conbus backfill --apply
conbus install --apply     # 레코더를 다시 켭니다
```

레코더가 꺼져 있던 동안 실시간 세션에서 **놓친 턴**은 레코더가 켜질 때 자동으로
채워집니다. Claude Code 트랜스크립트, Codex 스레드, 채팅 내보내기에서 가져옵니다.
시작 시 이 작업에는 최대 15초를 씁니다(`CONBUS_GAP_BUDGET_MS`로 바꿀 수 있습니다).
예산이 부족해 다 채우지 못한 범위는 `cache/gap-since`에 기억했다가 다음 시작 때
이어서 채우고, `daemon.log`에도 남깁니다. `conbus backfill --gaps`는 같은 빈틈을
시간 제한 없이 필요할 때 채우며, Claude Code에 대해 전체 기록에 걸쳐 동작합니다.
`--apply`를 붙이면 이것 역시 레코더를 멈춰야 합니다. `cache/`를 지우면 기억한
범위도 사라지므로 그 뒤에는 `conbus backfill --gaps`를 실행하세요.

**Claude.ai 대화**는 데스크톱 앱에 훅이 없습니다. Claude.ai에서 데이터를
내보내고(Settings → Privacy → Export data) 그 zip(또는 `conversations.json`)을
`~/.conbus/chat-export/`에 넣으면 레코더가 가져갑니다.
`conbus backfill --chat-export PATH`는 어디에 있는 파일이든 읽고, `--apply`를 붙이고
레코더를 멈춘 상태에서는 기록합니다. Claude의 Inference hooks를 쓰는 조직은 그것을
`conbus hook --inference-listen`(기본 `127.0.0.1:8787`)에 연결해 대화를 실시간으로
기록할 수 있습니다. 이 리스너는 인증을 하지 않으므로 루프백 주소만 받습니다. 앞에 두는
HTTPS 종단도 같은 기계에서 돌려야 합니다.

---

## 10. 체크아웃끼리 공유하는 사실

사실(fact)은 그 프로젝트의 모든 새 세션이 알아야 할, 확인된 짧은 진술입니다 —
"결제는 항상 큐를 거친다, 직접 호출하지 않는다".

```bash
conbus arena fact --workspace ~/src/shop --claim "payments go through the queue" \
  --anchor src/payments/queue.rs --type constraint
conbus arena link --from ~/src/shop --to ~/src/shop-admin
```

`--type`은 `decision`, `constraint`, `state`, `pointer`, `open` 중 하나입니다.
받아들인 사실은 그 폴더의 브리핑에 나타나고, `link`는 한 폴더의 사실을 다른
폴더에서도 보이게 합니다.

---

## 11. 내 데이터

### 저장 위치

`$CONBUS_HOME`이 있으면 거기, 없으면 `$XDG_STATE_HOME/conbus`, 그것도 없으면
`~/.conbus`입니다.

| 경로 | 내용 |
| --- | --- |
| `log/segment-*.jsonl` | 로그. 덧붙이기만 하는 JSON 줄, 8 MiB 단위 세그먼트 |
| `drops.jsonl` | 기록하지 못한 이벤트와 그 이유 |
| `daemon.log` | 레코더 자신의 메시지. 시각이 찍혀 있습니다 |
| `daemon.build` | 지금 도는 레코더가 어떤 빌드인지 |
| `daemon.lock` | 비어 있는 파일. 도는 레코더가 여기에 잠금을 걸어 두 번째 데몬이 아무것도 쓰지 못하게 합니다. **지우지 마세요.** 시작할 때 다른 레코더가 있다고 나와도 마찬가지입니다. 잠금을 쥔 쪽이 데몬보다 오래 살아남아 아직 쓰고 있는 시작 복구일 수 있고, 이 복구는 2분 안에 스스로 포기합니다. 복구가 도는 중에 파일을 지웠더니 다음 데몬이 그 옆에서 함께 써서 중복 `seq` 3개가 생겼고, 복구한 턴이 모두 두 번씩 영구히 남았습니다 |
| `conbus.sock` | 레코더의 소켓(모드 `0600`) |
| `quota/` | 마지막 쿼터 판독값 |
| `takeover.json` | `conbus run`을 기다리는 hop이 있을 때만 |
| `policy.json` | 직접 쓴 `PreToolUse` 규칙. 없으면 정책이 꺼져 있습니다(15장) |
| `cache/meta/` | 파생 읽기 캐시. 툴 본문을 SHA-256 요약으로 바꾼 레코드입니다. 지워도 잃는 것이 없습니다 |
| `spool/` | 레코더가 재시작하는 동안 훅이 맡겨 둔 요청. 다음 레코더가 읽어 기록한 뒤 지웁니다 |
| `facts.jsonl`, `links.jsonl` | 10장 |
| `chat-export/` | 넣어 둔 Claude.ai 내보내기 |

이 폴더의 내용은 본인 계정만 읽을 수 있습니다. 폴더는 `0700`, 로그 세그먼트는
`0600`입니다. 0.2.5 이전 빌드는 이를 `0755`와 `0644`로 만들어 이 Mac의 다른 계정도
읽을 수 있었으며, 레코더가 시작할 때 폴더를 닫습니다. 닫지 못하면 `daemon.log`에
그렇게 남기며, 로그 폴더는 어느 경우에도 닫힙니다.

소켓 경로는 104바이트보다 짧아야 합니다. `CONBUS_HOME`이 너무 깊으면 `doctor`가
알려 줍니다.

### 얼마나 남기는가

레코더가 도는 환경에 `CONBUS_RETENTION`을 설정합니다.

| 값 | 프롬프트와 답변 | 툴 입력과 출력 |
| --- | --- | --- |
| *(설정 안 함)* | 남김 | 쓰지 않음 — 크기만 |
| `full` | 남김 | 남김(본문마다 16 KiB에서 자름) |
| `encrypted_tools` | 남김 | 로그인 키체인에 있는 키로 AES-256-GCM 봉인. `conbus log --reveal`로 엽니다 |
| `ephemeral` | 아무것도 쓰지 않음 | 아무것도 쓰지 않음 |

어느 수준에서든 16 KiB보다 긴 본문 하나는 로그에서 잘리고, 원래 크기가 함께
기록됩니다.

레코더는 launchd로 돌기 때문에 "레코더가 도는 환경"은 그 plist입니다. 거기에 넣은
값은 `conbus install --apply`가 그대로 둡니다.

```bash
/usr/libexec/PlistBuddy -c 'Add :EnvironmentVariables dict' \
  -c 'Add :EnvironmentVariables:CONBUS_RETENTION string encrypted_tools' \
  ~/Library/LaunchAgents/sh.conbus.daemon.plist
conbus install --apply   # 그 값으로 레코더를 다시 띄움
```

수준은 그때부터 적용되고, 그 전에 쓰인 기록은 그대로입니다.

### Mac 밖으로 나가는 것

conbus에는 서버도, 계정도, 텔레메트리도 없습니다. 기능 세 가지가 내 로그의 글을
에이전트 세션에 넣고, 그 에이전트의 벤더는 다른 맥락과 똑같이 그것을 받습니다.

- **브리핑**(6장) — `CONBUS_BRIEFING=off`로 멈춥니다.
- **hop**(7장) — 지금까지의 대화를 들고 다음 레그를 엽니다.
- `conbus summary --bus-id ID` — *다른* 레그의 에이전트에게 대화 요약을 부탁합니다.
  실행할 때만 동작합니다.

`conbus usage --otlp URL`은 글이 아니라 숫자(`--shareable`의 줄)만, 지정한 이 Mac의
컬렉터로, 실행할 때만 보냅니다(5장). 그다음 어디로 가는지는 그 컬렉터의 설정에 달려
있습니다.

쿼터를 읽으려고 conbus는 에이전트 자신의 CLI를 실행합니다(8장). hop을 결정할 때는
`gh`에 그 폴더의 풀 리퀘스트를 묻습니다.

### conbus가 절대 건드리지 않는 것

벤더 자격 증명과 토큰. 에이전트의 세션 파일(`backfill`을 실행할 때와 레코더가
켜지며 빈틈을 채울 때는 예외). `~/Downloads` 폴더.

### 지우기

`rm -rf ~/.conbus`는 conbus가 기록한 모든 것을 지웁니다. `uninstall`은 이것을 대신
하지 않습니다.

---

### `conbus sync` (experimental)

`conbus sync`는 초대받은 서비스에 하루 단위 집계 행(`usage --shareable`이 보여 주는
것과 같은 숫자, 폴더 이름 없음)만 보내는 클라이언트입니다. 아직 서비스는 없고,
[RFD 0002](rfd/0002-metadata-sync.md)와 [RFD 0003](rfd/0003-security-design.md)은
초안입니다. `conbus sync --join INVITE`는 초대를 보여 주기만 하고, `--apply`를 붙여야
기기 키를 로그인 키체인에 저장합니다. `conbus sync --off`는 서비스에 닿지 않아도 기기
키를 지웁니다. 로그 자체는 이 기기를 떠나지 않습니다.

## 12. 업그레이드

설치 스크립트를 다시 실행하고 레코더를 다시 시작하세요.

```bash
curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
conbus install --apply
```

두 번째 명령은 이미 등록된 설정을 바꾸지 않습니다. 레코더를 새 바이너리로 다시
시작할 뿐이고, 몇 초 걸립니다. `doctor`에 `daemon … DIFFERS`가 보이면 이 단계를
건너뛴 것입니다. 지금 도는 레코더보다 새로운 훅은 반쯤 알아듣는 대신
"Restart the daemon"이라며 거부됩니다.

이 명령은 레코더를 재시작하지 않고 내렸다가 다시 올립니다(`bootout` 후
`bootstrap`). macOS는 에이전트를 올릴 때 교체된 바이너리를 확인하고, 그냥
재시작하면 한 번 거부하기 때문입니다. 레코더가 뜨는 몇 초 동안 훅은 기록되지
않습니다.

---

## 13. 제거

```bash
conbus uninstall          # 무엇을 지울지
conbus uninstall --apply  # 지우기
```

- conbus를 실행하는 훅 항목은 모든 에이전트 설정에서 빠지고, 다른 것은 빠지지
  않습니다. 같은 블록에 있던 여러분의 훅도 그대로 남습니다.
- StatusLine은 원래 명령으로 돌아갑니다.
- opencode 플러그인, grok 훅 파일, launchd 에이전트(레코더를 멈춘 뒤),
  `~/.local/bin/conbus`를 지웁니다.
- 다시 쓴 설정 파일마다 `…json.conbus-uninstall-backup`을 남깁니다.
- Codex는 승인을 위치로 기억합니다. 다른 훅이 한 칸씩 앞으로 당겨지면 uninstall이
  알려 주고, Codex가 그 훅을 다시 승인해 달라고 합니다.
- **로그는 남깁니다.** 지우고 싶으면 `~/.conbus`를 직접 지우세요.

---

## 14. 문제 해결

**`conbus: command not found`** — `PATH`에 `~/.local/bin`을 추가하세요. 셸 프로필에
`export PATH="$HOME/.local/bin:$PATH"`.

**macOS가 바이너리를 "열 수 없다"고 함** — 브라우저로 받아서 격리(quarantine) 표시가
붙었습니다. `xattr -d com.apple.quarantine conbus`를 실행하거나, `curl`을 쓰는
`install.sh`로 설치하세요.

**`socket … not answering`** — 레코더가 돌고 있지 않습니다. `conbus install --apply`로
켭니다. 계속 멈춘다면 `~/.conbus/daemon.log`에 이유가 있습니다. launchd 에이전트가
로드된 상태에서 `conbus daemon`을 손으로 띄우지 마세요. 둘이 소켓을 두고 다툽니다.

**`socket … UNUSABLE — the socket path … is N bytes`** — `CONBUS_HOME`을 더 짧은
디렉터리로 설정하세요.

**`hooks … CANNOT RUN`** — 등록된 바이너리가 없거나 망가졌습니다.
`conbus install --apply`를 다시 실행하세요.

**`codex … NOT TRUSTED` 또는 `MODIFIED`** — Codex를 켜고 보여 주는 훅을 승인하세요.
`MODIFIED`는 다른 도구가 Codex의 `hooks.json`을 고쳤을 때도 나타납니다.

**`drops`가 계속 늘어남** — `doctor`가 보여 주는 마지막 이유를 보세요.
`SessionEnd`의 `no conversation for this session`은 첫 프롬프트 전에 끝난 세션이라면
정상입니다. `names no session`은 에이전트가 어느 세션인지 밝히지 않은 이벤트를
보냈다는 뜻이고, conbus는 추측하는 대신 그것을 버립니다.

**`quota claude … StatusLine never ran`** — 감싼 StatusLine을 읽도록 터미널 `claude`를
다시 시작하세요. 데스크톱 앱은 StatusLine을 실행하지 않습니다(8장).

**`takeover  stale → …`** — 기다리는 `conbus run`이 없을 때 hop이 일어났습니다.
다음 `conbus run`이 그것을 버립니다. 따로 할 일은 없습니다.

**`nameless  <bus id> — N records…`** — 0.1.0 이전 빌드가 세션을 밝히지 않은
이벤트를 공용 대화 하나 아래 묶었습니다. 0.1.0부터는 그런 이벤트를 거부합니다.
로그는 덧붙이기만 하므로 예전 레코드는 남아 있고, 이 줄이 그것이 무엇인지 알려
줍니다.

**한도에 걸렸는데 hop이 일어나지 않음** — `conbus status`에서 규칙 줄, 목적지의
쿼터, 그 세션이 프로그램이었는지(무인 세션은 hop하지 않습니다)를 확인하세요.
`conbus hop`은 지금이라면 어떻게 결정할지 보여 줍니다.

---

## 15. 로컬 정책

데이터 폴더의 `policy.json`은 내가 직접 정하는 `PreToolUse` 결정 목록입니다. 파일이
없으면 정책은 꺼져 있고, 모든 툴 호출은 벤더가 원래 정하는 대로 지나갑니다. 파일이
있으면 레코더가 아니라 훅이 호출을 실행하기 전에 규칙과 맞춰 봅니다. 그래서
레코더가 멈춰 있어도 거부(deny)는 풀리지 않습니다.

```json
{
  "version": 1,
  "rules": [
    {
      "harness": "claude_code",
      "tool": "Bash",
      "input_contains": ["rm -rf /"],
      "decision": "deny",
      "reason": "no agent wipes this disk"
    }
  ]
}
```

- `harness`는 생략할 수 있습니다. 생략하면 결정을 받는 모든 에이전트, 지금은 Claude
  Code와 Codex에 적용됩니다. Cursor의 응답 계약에는 결정이 없으므로 Cursor 호출에는
  적용되지 않습니다.
- `tool`은 페이로드의 `tool_name`을 그대로 씁니다. `"*"`는 모든 툴입니다.
- `input_contains`는 부분 문자열 목록입니다. 툴 입력을 압축한 JSON에 하나라도 있으면
  규칙이 맞습니다. 생략하면 모든 입력이 맞습니다.
- `decision`은 `deny` 또는 `ask`입니다. `allow`는 일부러 없습니다. 정책은 실행되는
  것을 조일 수만 있고, 벤더가 띄웠을 확인 창을 없앨 수는 없습니다.
- `reason`은 사용자에게 보이므로 반드시 써야 하고 비울 수 없습니다.

규칙은 순서대로 모두 맞춰 봅니다. 맞는 `deny`는 맞는 `ask`보다 항상 앞서고, 같은
결정끼리는 먼저 나온 규칙의 이유가 쓰입니다. 파싱되지 않거나 버전이 다르거나 모르는
필드가 있는 파일은 아무것도 결정하지 않습니다. 이때 `conbus doctor`가 그렇다고
말하고, `conbus policy check`는 규칙이나 오류를 출력합니다. `conbus policy test
--harness claude_code --tool Bash --input '{"command":"ls"}'`는 호출 하나에 파일이
무엇을 할지 실행하지 않고 보여 줍니다.

결정은 로그에 남지 않습니다. 툴 요청은 늘 그랬듯 남습니다.

## 16. Claude Code 프로필: 손으로 바꾸는 두 번째 계정 (experimental)

Claude Code는 여러 계정을 나란히 쓸 수 있습니다. 프로필 `NAME`은
`$HOME/.claude-NAME` 디렉터리이고, `CLAUDE_CONFIG_DIR`가 `claude`에게 어느 것을
쓸지 알려 줍니다. conbus는 정확히 그것만 지원합니다. 내 계정을, 벤더의 로그인 절차로
로그인해서, 내가 말할 때만 바꿉니다.

```bash
# 1. 프로필을 만들고 로그인합니다. conbus가 아니라 벤더의 절차입니다:
CLAUDE_CONFIG_DIR=$HOME/.claude-work claude
/login

# 2. 같은 훅과 StatusLine 감싸기를 프로필의 settings.json에 등록합니다:
conbus install --apply --profile work

# 3. 프로필로 실행합니다:
conbus run claude_code --profile work
```

프로필 이름은 `[a-z0-9-]+`입니다. `$HOME/.claude-NAME`이 아직 없으면
`install --profile`은 거절하고 로그인 명령을 보여 줍니다. conbus는 그 디렉터리를
만들지 않고, 누구도 로그인시키지 않으며, 그 안에서 `settings.json` 말고는 읽지
않습니다. `uninstall --profile NAME`은 그 파일에서 등록만 걷어 내고 기본 설정은
건드리지 않습니다.

프로필 세션의 레코드는 출처 스탬프에 `profile`을 담고, 프로필의 StatusLine 판독값은
따로 저장됩니다. `conbus quota`는 이를 `claude_code@NAME`으로 보여 주므로, 라우팅은
대화가 실제로 도는 계정의 쿼터를 읽습니다. 백필과 공백 채우기는 기본 프로필에서만
합니다.

**바꾸는 것은 손으로만 합니다.** 벤더 약관은 사용 한도를 우회하는 것을 금지합니다.
그래서 conbus는 한도에 걸렸다고 프로필을 바꾸지 않습니다. 어떤 쿼터 신호나 라우트,
hop도 대화를 내 계정 하나에서 다른 계정으로 옮기지 않습니다. 대화의 마지막 Claude
구간이 프로필로 돌았다면, 자동 hop은 기본 계정으로 돌아가는 대신 기다리고 그렇다고
말합니다. `conbus go claude_code`(기본 계정, 내 선택)나 `conbus run claude_code
--profile NAME` 중에서 고르면 됩니다.

## 17. 레퍼런스

### 명령

| 명령 | |
| --- | --- |
| `conbus install [--apply]` | 설치된 모든 에이전트에 훅을 등록. `--apply`가 없으면 보고만 |
| `conbus install --managed` | Claude Code 관리 설정에 넣을 훅 블록(stdout의 JSON)과 넣을 곳을 출력. 아무것도 쓰지 않음([Managed Macs](fleet.md)) |
| `conbus uninstall [--apply]` | 모두 다시 빼기. 로그는 남김 |
| `conbus doctor` | 무엇이 연결됐고 무엇이 안 됐는지 |
| `conbus status` | 라우팅 규칙, 쿼터, hop 기록, 비용 |
| `conbus quota` | 각 레그에 남은 양 |
| `conbus log [--json] [--bus-id ID] [--since TS] [--tail N] [--grep TEXT] [--reveal]` | 로그 읽기 |
| `conbus hop [HARNESS] [--from ID] [--to HARNESS] [--apply] [--demo]` | hop 미리보기, `--apply`면 실행 |
| `conbus go [HARNESS] [--from ID]` | 지금 대화에 대한 `hop --apply` |
| `conbus run [HARNESS]` | 이 터미널에서 에이전트 시작. hop이 다음 에이전트를 여기서 띄움 |
| `conbus backfill [--apply] [--gaps] [--chat-export PATH]` | 지난 기록 되살리기 |
| `conbus summary --bus-id ID` | 다른 레그의 에이전트에게 대화 요약 요청 |
| `conbus usage [--days N] [--since DAY] [--day DAY] [--json] [--shareable] [--otlp URL]` | 에이전트가 한 일을 셈. `--otlp`는 `--shareable`의 줄을 이 Mac의 컬렉터로 보냄(5장) |
| `conbus progress [--days N] [--since DAY] [--day DAY] [--workspace DIR] [--json]` | 같은 숫자를 폴더별로: 거기서 일한 에이전트, 에이전트 사이의 넘김, `HEAD`에 쌓인 커밋(5장) |
| `conbus verdict` | hop의 4주 시험 채점(아래) |
| `conbus arena fact --workspace DIR --claim TEXT --anchor PATH [--type T]` | 사실 받아들이기 |
| `conbus arena link --from DIR --to DIR` | 한 폴더의 사실을 다른 폴더와 공유 |
| `conbus daemon` | 레코더(launchd가 실행) |
| `conbus hook [--harness NAME] [--statusline] [--inference-listen [ADDR]]` | 에이전트가 부르는 훅 |
| `conbus --version` | 버전과 커밋 |

`conbus <command> --help`로 사용법을 볼 수 있습니다.

### 환경 변수

| 변수 | 효과 |
| --- | --- |
| `CONBUS_HOME` | conbus가 데이터를 두는 곳(11장) |
| `XDG_STATE_HOME` | `CONBUS_HOME`이 없을 때 `$XDG_STATE_HOME/conbus`를 씀 |
| `CONBUS_RETENTION` | `full`, `encrypted_tools`, `ephemeral`(11장) |
| `CONBUS_BRIEFING` | `off`면 브리핑을 멈춤 |
| `CODEX_HOME` | Codex가 설정을 두는 곳(기본 `~/.codex`) |
| `CONBUS_CLAUDE_BIN`, `CONBUS_CODEX_BIN`, `CONBUS_CURSOR_BIN`, `CONBUS_CURSOR_APP_BIN`, `CONBUS_GROK_BIN`, `CONBUS_OPENCODE_BIN`, `CONBUS_AGY_BIN` | 그 에이전트에 다른 바이너리를 씀 |
| `CONBUS_GH` | 다른 `gh`를 씀 |

### `conbus verdict`

conbus는 가설 하나에서 출발했습니다. 여러 에이전트를 쓰는 사람은 자동 hop이 의미
있을 만큼 자주 한도에 걸린다는 것입니다. `verdict`는 여러분의 Mac에서 28일 동안의
실제 사용으로 그 가설을 채점합니다. 스스로 일어난 hop만 세고 — 직접 입력했거나
테스트로 연출한 hop은 절대 세지 않고 — 결과를 이렇게 읽습니다. 라우팅 hop이 주
3회 미만이면 라우팅을 쓸 이유가 없다는 뜻이고, 창이 닫혔는데 hop이 하나도 없으면
판정 불가입니다. 28일이 지나기 전에는 그렇다고만 말하고 아무것도 채점하지 않습니다.

---

## 18. 알려진 한계

- macOS와 Linux만 지원합니다. Linux에서 실제 벤더 세션은 아직 검증하지 않았습니다([지원 표](support-matrix.md)).
- Claude의 쿼터는 터미널 `claude`의 StatusLine에서만 보입니다.
- 자동 hop은 아직 실제 쿼터 중단에서 발동한 적이 없습니다.
- 일부 Cursor 세션은 어느 세션인지 밝히지 않은 툴 이벤트를 보냅니다. conbus는
  추측하지 않고 그것을 버립니다.
- Codex 훅은 Codex 안에서 승인해야 합니다.
- v0.2.6 이전 릴리스는 공증(notarize)되지 않았습니다(브라우저 다운로드는 14장 참고).
  v0.2.6부터는 Developer ID로 서명하고 공증합니다.
- Cursor, grok, opencode는 남은 쿼터를 알려 주지 않으므로, conbus가 이들의 한도에
  앞서 hop을 계획할 수 없습니다.
