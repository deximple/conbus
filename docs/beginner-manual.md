# Conbus 비기너 실작동 매뉴얼

이 문서는 설계가 아니라 **이 머신에서 켜고, 쓰고, 갈아끼우는 순서**다.

Conbus는 채팅 앱이 아니다. Claude Code / Codex TUI를 그대로 쓰면서, 한 대화(`bus_id`)만 유지한 채 퀴터가 떨어지면 같은 터미널에서 하네스를 갈아끼우는 **로컬 라우팅 레이어**다. 세션 파일을 변환해 다른 CLI를 띄우거나, MCP로 서로를 호출하는 다리가 아니다.

로그인·토큰은 벤더 바이너리가 한다. Conbus는 읽지도 저장하지도 않는다.

## 한 줄

강점: 이미 쓰는 두 구독의 연속성.
약점: macOS + 훅 + 데몬에 묶인 운영, 그리고 “같은 대화처럼 보이게 만드는 것”과 “진짜 같은 모델 컨텍스트”의 간극.

한 대화가 `bus_id`를 유지한 채 하네스만 바뀐다. Claude 퀴터가 끝나면 같은 터미널에서 Codex가 다음 턴을 이어 받고, 이미 답한 것을 다시 묻지 않는 것이 목표다. 훅·데몬·Codex trust·`conbus run` 중 하나라도 빠지면 조용히 일반 Claude/Codex로 돌아간다. 프롬프트는 살아 있다.

## 누구에게 맞나

맞다: macOS에서 Claude Code를 주력으로 쓰고, Codex 구독이 이미 있으며, 5시간/주간 퀴터에 자주 걸리고, `conbus run`을 부모로 둘 수 있는 사람.

아니다: Linux/Windows, 벤더 CLI를 직접 켜는 습관, 훅을 자주 고치는 사람, 세션 파일을 다른 도구로 lossless 이전하려고 하는 사람, 데몬/소켓/trust를 보기 싫은 사람.

자동 hop은 신호가 있을 때만 한다. Claude는 StatusLine stdin의 `rate_limits`(벤더가 이미 채워 넣음)와 `StopFailure`, Codex는 `account/rateLimits/read`. 우리는 `oauth/usage`를 직접 치지 않는다. Cursor·Agy·OpenCode·Grok은 `--to`로 핸드셰이크 hop이 되고, 자동 overflow는 그쪽에 남은 퀴터 JSON이 잡히기 전에는 Codex로만 간다. TUI `/usage`를 긁지 않는다.

## 같은 `bus_id` ≠ 같은 컨텍스트 창

버스가 잇는 것은 대화 id와 기록이다. Claude의 토큰 창을 Codex가 그대로 상속한다는 뜻이 아니다. hop이 여는 Claude warm은 `--max-turns 0`이라 모델 턴이 아니고, 이어받기 품질은 그때 넣는 팩(브리핑·백필)에 달렸다. `inject`는 그 비용이다. 성공 지표가 아니다.

`status`의 `routed …`와 `continued`가 같은 `bus_id`면 **라우팅은 성공**한 것이다. 모델이 전부 아는지는 `log --bus-id`로 팩과 다음 턴을 본다.

## 하지 말 것

이 네 줄이 깨지면 훅이 조용히 죽는다. 에러가 안 보이는 것이 기본이다.

- 설치된 바이너리를 **그 자리에서** 덮어쓰지 않는다. 옆에 쓴 뒤 `mv`한다.
- 사용자가 연 Codex 스레드에 쓰지 않는다. hop이 여는 `warm:` 스레드만 우리 것이다.
- Codex 훅 trust 해시를 파일에 넣지 않는다. Codex 첫 화면에서 직접 승인한다.
- `~/.cursor/hooks.json`에 Conbus를 등록하지 않는다. Cursor는 이미 Claude 훅 경로로 들어오며, 네이티브 등록은 이중 기록이다.

`conbus hook`이 실패해도 프롬프트는 막히지 않는다. 떨어진 기록은 `~/.conbus/drops.jsonl`과 `conbus doctor`에만 남는다.

## 준비물

- macOS. 데몬은 `launchd` 에이전트 `sh.conbus.daemon`이다.
- 이미 로그인된 `claude`, `codex`. PATH에서 보이면 된다.
- 빌드하려면 `cargo` (이 저장소는 edition 2024).
- 설치 위치는 `~/.local/bin/conbus`. 훅은 PATH가 아니라 **이 절대 경로**를 부른다.

데이터는 `$XDG_STATE_HOME/conbus`가 있으면 거기, 없으면 `~/.conbus/`.

## 1회 설치

이미 이 머신에 돌아가고 있으면 이 장을 건너뛰고 `conbus doctor`만 본다.

### 1. 바이너리

```bash
cd /Applications/conbus
cargo build --release -p conbus
mkdir -p ~/.local/bin ~/.conbus
cp -f target/release/conbus ~/.local/bin/conbus.next
mv -f ~/.local/bin/conbus.next ~/.local/bin/conbus
```

`cp`로 `~/.local/bin/conbus`를 직접 덮으면, 돌아가는 훅이 exit 137로 죽는다.

### 2. 데몬

`~/Library/LaunchAgents/sh.conbus.daemon.plist`를 만든다. `$HOME`만 본인 경로로 바꾼다.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>sh.conbus.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/YOU/.local/bin/conbus</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/Users/YOU/.conbus/daemon.log</string>
  <key>StandardErrorPath</key><string>/Users/YOU/.conbus/daemon.log</string>
</dict>
</plist>
```

```bash
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/sh.conbus.daemon.plist
```

이미 올라가 있으면 `bootstrap` 대신 재시작만 한다.

```bash
launchctl kickstart -k "gui/$(id -u)/sh.conbus.daemon"
```

수동으로 `conbus daemon`을 띄운 채로 에이전트를 올리면 소켓을 못 잡고 `KeepAlive`가 재시작 루프에 들어간다. 수동 프로세스를 먼저 끈다.

### 3. 훅 등록

```bash
conbus install
conbus install --apply
```

첫 줄은 미리보기, 둘째가 기록이다. 이미 있는 훅은 그대로 두고 Conbus만 붙인다.

- Claude: `~/.claude/settings.json`
- Codex: `$CODEX_HOME/hooks.json` (`settings.json`이 아니다)

### 4. Codex 승인

Codex를 한 번 켠다. 시작 화면에 훅 검토가 뜨면 **Trust all and continue**다.
Conbus가 그 해시를 쓰면 동의를 위조하는 것과 같다. `doctor`가 `untrusted`면 아직 한 줄도 안 남는다.

### 5. 건강한지

```bash
conbus doctor
conbus --version
```

볼 줄:

| 줄 | 정상 |
| --- | --- |
| `daemon` | 지금 돌아가는 빌드와 pid |
| `hooks` / `events` | Claude·Codex가 등록됨. Codex는 `trusted` |
| `last` | 방금 켠 세션의 시각이 최근 |
| `drops` | 설치 직후가 아니면 늘어나지 않음 |
| `route` | `stay on …` 또는 `hop …` |
| `warm` | 아직 hop 전이면 `none`이어도 된다 |

`events … MISSING`이면 `conbus install --apply`를 다시 한다.
Codex가 `untrusted`면 4번으로 돌아간다.
`last`가 몇 시간 전인데 방금 세션을 열었다면 훅이 안 붙은 것이다.

## 매일

핵심 가치는 이 습관 하나에 있다. 같은 터미널에서 하네스가 바뀌려면 **벤더를 직접 켜지 말고** 부모를 Conbus로 둔다. `claude`를 그대로 켜면 기록은 되어도 hop 때 창이 안 바뀐다.

```bash
cd /path/to/the/repo
conbus run claude_code
```

보이는 창은 평소 Claude다. 퀴터에 걸리면 데몬이 `hop --apply`를 띄우고, 이 프로세스가 Codex warm을 같은 TTY에서 이어 받는다.

Codex로 이미 와 있으면:

```bash
conbus run codex
```

벤더를 직접 켜도 기록은 된다. 다만 그 창은 hop 때 자동으로 바뀌지 않는다.

## 퀴터가 떨어졌을 때

Claude `StopFailure` `error=rate_limit`가 오면 데몬이 한 번 hop한다. 사람이 `hop`을 또 누를 필요는 없다.

그 다음:

```bash
conbus status
conbus log --bus-id <bus_id> --tail 20
```

`status`의 `history`에 `routed Displaced → codex`와 `continued`가 같은 `bus_id`로 있으면 라우팅은 된 것이다.
`warm` 줄에 Codex 스레드 id가 보인다. 그건 버스가 연 스레드다. 다음 Codex 턴이 이미 답한 것을 다시 물으면 팩이 얇은 것이지, hop이 실패한 것은 아니다.

직접 반대쪽으로 돌리려면:

```bash
conbus hop --from <bus_id> --to claude_code
conbus hop --apply --from <bus_id> --to claude_code
```

`--from`은 **전체 `bus_id`**를 붙인다. 앞 몇 글자는 같은 초에 민트된 다른 대화와 겹친다.
`--apply` 없이 치면 결정만 보여 준다.

hop은 양쪽 warm을 연다. Claude 쪽은 `--max-turns 0`이라 모델 턴이 아니다.
`conbus run`을 쓰고 있으면 takeover가 그 창에서 다음 레그를 exec한다.

## 상태 보기

채팅이 아니다. 숫자와 마지막 hop만 본다.

```bash
conbus status          # 규칙, 퀴터, hop 히스토리, 비용
conbus quota           # Codex 프로브 + Claude StatusLine 캐시 + 카탈로그
conbus log --tail 30
conbus log --bus-id <bus_id> --tail 20
conbus log --json --bus-id <bus_id> --tail 1
```

`status`의 `hops routing N · failover M`이 `doctor`와 같아야 한다.
`inject`는 비용이다. 성공 지표가 아니다.

## 지난 대화를 가져오기

데몬이 죽은 동안의 턴은 재시작 때 같은 `bus_id`로 메워진다. 미리 보고 싶으면:

```bash
conbus backfill --gaps
```

`--gaps --apply`는 데몬이 켜져 있으면 거절한다. 재시작이 그 일이다.

설치 전 히스토리(처음 한 번)는 데몬을 **완전히 끈 뒤**에만 `--apply`한다.

```bash
launchctl bootout "gui/$(id -u)/sh.conbus.daemon"
# 소켓이 죽을 때까지 기다린다. 바로 --apply 하면 "daemon is listening"으로 거절된다.
conbus backfill          # 미리보기
conbus backfill --apply
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/sh.conbus.daemon.plist
```

라이브 세션을 백필이 다시 민트하지 않는다.  Twin이 보이면 `doctor`가 어느 쪽이 살아있는 바인딩인지 말한다.

## 바이너리 갈아끼우기

```bash
cargo build --release -p conbus
cp -f target/release/conbus ~/.local/bin/conbus.next
mv -f ~/.local/bin/conbus.next ~/.local/bin/conbus
launchctl kickstart -k "gui/$(id -u)/sh.conbus.daemon"
conbus doctor
```

`doctor`의 `daemon build`가 `conbus --version`과 같아야 한다.

훅 등록 내용을 바꾸면 Codex trust가 `modified`로 떨어지고 다시 조용히 안 돈다. 그때는 Codex를 다시 켜서 승인한다. 훅을 앞에 끼워 넣으면 키 인덱스가 밀려 trust가 유실된다.

## 워크스페이스 fact (선택)

채팅이 아니다. 확인한 한 문장을 링크된 다른 워크스페이스의 다음 팩에 넣는다.

```bash
conbus arena fact --workspace /path/A --claim "한 문장, 현재형" --anchor crates/foo.rs
conbus arena link --from /path/A --to /path/B
```

B에서 다음 프롬프트를 치면 팩에 `arena` 줄이 보인다. 이 에이전트 환경에 `CONBUS_BRIEFING=off`가 있으면 주입이 잠긴다. 확인할 때는 그 변수를 뺀다.

## 막히면

| 증상 | 먼저 볼 것 |
| --- | --- |
| 방금 친 턴이 로그에 없다 | `doctor`의 `daemon`, `last`, `drops`. 바이너리를 in-place로 덮었는지 |
| Codex만 안 남는다 | Codex를 켜서 훅을 다시 trust. `async: true`를 넣지 않았는지 |
| hop이 `stay on claude_code`인데 Codex로 가고 싶다 | `--from`에 전체 id. `--to codex` |
| hop은 됐는데 창이 안 바뀐다 | 그 창을 `conbus run`으로 열었는지. 직접 켠 벤더는 안 바뀐다 |
| `this conversation is already leased` | 다른 hop이 끝나기를 기다리거나, 그 대화의 `session_ended`가 로그에 있는지 |
| doctor는 건강한데 브리핑이 없다 | `CONBUS_BRIEFING=off`인지. off는 기록은 하고 말은 안 한다 |

데몬 로그는 `~/.conbus/daemon.log`.
떨어뜨린 훅은 `~/.conbus/drops.jsonl`.

## 명령 한 장

```text
conbus doctor
conbus status
conbus run claude_code
conbus hop --from <bus_id>
conbus hop --apply --from <bus_id> --to claude_code|codex|cursor|agy|opencode|grok
conbus log --bus-id <bus_id> --tail 20
conbus quota
conbus install --apply
conbus backfill --gaps
conbus --version
```
