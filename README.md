# Claude-Pilot

> AI 코딩 오케스트레이터 - 증거 기반 계획, 멀티 에이전트 합의, 수렴적 품질 검증으로 완벽한 코드를 만듭니다.

## 왜 Claude-Pilot인가?

| 기존 AI 코딩 | Claude-Pilot |
|-------------|--------------|
| ❌ 추측으로 코딩 | ✅ 증거 수집 후 계획 |
| ❌ "완료!" 후 버그 발견 | ✅ 2회 연속 검증 통과까지 반복 |
| ❌ 복잡한 작업에서 일관성 부족 | ✅ 멀티 에이전트 합의로 조율 |
| ❌ 중단되면 처음부터 다시 | ✅ 이벤트 소싱으로 중단점에서 재개 |

---

## 목차

- [빠른 시작](#빠른-시작)
- [핵심 개념](#핵심-개념)
- [실행 흐름](#실행-흐름)
- [CLI 명령어](#cli-명령어)
- [설정](#설정)
- [멀티 에이전트 시스템](#멀티-에이전트-시스템)
- [고급 기능](#고급-기능)
- [실제 사용 사례](#실제-사용-사례)
- [문제 해결](#문제-해결)

---

## 빠른 시작

### 요구사항

- Rust 1.92.0 이상
- Git
- Claude Code CLI 설치 및 OAuth 인증 완료

### 설치

```bash
git clone https://github.com/anthropics/claude-pilot.git
cd claude-pilot
cargo build --release
cargo install --path .
```

### 첫 미션 실행

```bash
# 1. 프로젝트 초기화
cd your-project
claude-pilot init

# 2. 미션 시작
claude-pilot mission "사용자 인증 기능 추가"
```

### 격리 모드

```bash
# Worktree 격리 (권장) - 별도 디렉토리에서 작업
claude-pilot mission "버그 수정" --isolated

# 브랜치 격리 - 새 브랜치에서 작업
claude-pilot mission "리팩토링" --branch

# 직접 모드 - 현재 브랜치에서 바로 작업
claude-pilot mission "문서 업데이트" --direct
```

---

## 핵심 개념

### 아키텍처

```mermaid
flowchart TB
    subgraph Orchestration["오케스트레이션"]
        MO[MissionOrchestrator<br/>미션 생명주기]
    end

    subgraph Agents["에이전트 시스템"]
        AP[AgentPool<br/>에이전트 풀]
        CO[Coordinator<br/>조율/합의]
    end

    subgraph State["상태 관리"]
        ES[(EventStore<br/>이벤트 저장)]
        RP[Replay/Resume<br/>재개 지원]
    end

    MO --> AP
    MO --> CO
    CO --> ES
    ES --> RP
    AP --> CO
```

### 3가지 핵심 보장

| 보장 | 설명 | 구현 |
|------|------|------|
| **품질 보장** | 2회 연속 검증 통과 필수 | ConvergentVerifier |
| **일관성 보장** | 멀티 에이전트 합의 | Cross-Visibility Consensus |
| **내구성 보장** | 중단점에서 재개 가능 | Event Sourcing (SQLite) |

---

## 실행 흐름

```mermaid
flowchart TD
    Start["사용자 인증 기능 추가"] --> P1

    subgraph P1["Phase 1: Research"]
        R1[코드베이스 분석]
        R2[의존성 확인]
        R3[증거 수집]
    end

    P1 --> P2

    subgraph P2["Phase 2: Planning"]
        PL1[작업 분해]
        PL2[Cross-visibility로 제안 공유]
        PL3[합의 도출]
    end

    P2 --> P3

    subgraph P3["Phase 3: Implement"]
        I1[코드 생성]
        I2[파일 충돌 P2P 해결]
        I3[FileOwnership 동기화]
    end

    P3 --> P4

    subgraph P4["Phase 4: Verify"]
        V1[Build/Test/Lint]
        V2[코드 리뷰]
        V3[이슈 자동 수정]
    end

    P4 --> Check{클린?}
    Check -->|NO| P3
    Check -->|YES| Check2{2회 연속?}
    Check2 -->|NO| P4
    Check2 -->|YES| Done[완료!]
```

---

## CLI 명령어

### 미션 관리

| 명령어 | 설명 |
|--------|------|
| `claude-pilot init` | 프로젝트 초기화 |
| `claude-pilot mission "<설명>"` | 미션 시작 |
| `claude-pilot status [mission_id]` | 상태 확인 |
| `claude-pilot list` | 모든 미션 목록 |
| `claude-pilot logs <mission_id>` | 로그 확인 |

### 미션 제어

| 명령어 | 설명 |
|--------|------|
| `claude-pilot pause <mission_id>` | 일시정지 (체크포인트 저장) |
| `claude-pilot resume <mission_id>` | 중단점에서 재개 |
| `claude-pilot cancel <mission_id>` | 취소 |
| `claude-pilot retry <mission_id>` | 실패 지점부터 재시도 |

### 완료 후 작업

| 명령어 | 설명 |
|--------|------|
| `claude-pilot merge <mission_id>` | 메인에 병합 |
| `claude-pilot cleanup [mission_id]` | Worktree 정리 |

### 주요 옵션

```bash
--isolated              # Worktree 격리 모드 (권장)
--branch                # 브랜치 격리 모드
--direct                # 현재 브랜치에서 작업
--priority <P1|P2|P3>   # 우선순위 (P1=긴급)
-o json                 # JSON 출력
-o stream               # 실시간 스트리밍
-v, --verbose           # 상세 로그
```

---

## 설정

### 설정 파일 위치

```
your-project/
├── .pilot/
│   ├── config.toml       # 프로젝트 설정
│   └── events.db         # 이벤트 저장소
└── .claudegen/
    └── manifest.json     # 모듈 구조 정의
```

### 핵심 설정 (.pilot/config.toml)

```toml
# 기본 설정
[orchestrator]
max_iterations = 100
mission_timeout_secs = 604800  # 7일

# 멀티 에이전트
[multi_agent]
enabled = true
parallel_execution = true

[multi_agent.instances]
research = 1
planning = 3      # 합의용
coder = 2         # 병렬 구현
verifier = 1

# 합의 설정
[multi_agent.consensus]
max_rounds = 5
enable_cross_visibility = true
flat_threshold = 3        # 3명 이하 → 단일 라운드
hierarchical_threshold = 10

# 수렴적 검증 (변경 불가)
[recovery.convergent_verification]
required_clean_rounds = 2  # 필수
include_ai_review = true   # 필수

# 이벤트 저장소
[state]
database_path = ".pilot/events.db"
enable_snapshots = true
```

### 모듈 구조 정의 (.claudegen/manifest.json)

```json
{
  "project": {
    "name": "my-project",
    "modules": [
      {
        "id": "auth",
        "name": "인증 모듈",
        "paths": ["src/auth/"],
        "dependencies": ["models"],
        "responsibility": "사용자 인증 및 권한 관리"
      }
    ]
  }
}
```

---

## 멀티 에이전트 시스템

### 에이전트 역할

```mermaid
flowchart LR
    subgraph Phase1["Phase 1"]
        RA[ResearchAgent<br/>증거 수집]
    end

    subgraph Phase2["Phase 2"]
        PA[PlanningAgent ×3<br/>합의 계획]
    end

    subgraph Phase3["Phase 3"]
        CA[CoderAgent ×2<br/>병렬 구현]
    end

    subgraph Phase4["Phase 4"]
        VA[VerifierAgent<br/>검증]
        RV[ReviewerAgent<br/>리뷰]
    end

    Phase1 --> Phase2 --> Phase3 --> Phase4
```

### 계층형 합의

```mermaid
flowchart BT
    M[Tier 0: Module<br/>모듈별 에이전트] --> G[Tier 1: Group<br/>그룹 코디네이터]
    G --> D[Tier 2: Domain<br/>도메인 코디네이터]
    D --> W[Tier 3: Workspace<br/>워크스페이스]
    W --> CW[Tier 4: CrossWorkspace<br/>전체 프로젝트]
```

### Cross-Visibility 합의

```mermaid
flowchart LR
    subgraph Traditional["기존 방식"]
        A1[Agent A] --> V1[다수결]
        B1[Agent B] --> V1
        C1[Agent C] --> V1
        V1 --> R1[불일치 가능]
    end

    subgraph CrossVis["Cross-Visibility"]
        A2[Agent A] <--> Share[실시간 공유]
        B2[Agent B] <--> Share
        C2[Agent C] <--> Share
        Share --> R2[일관된 합의]
    end
```

---

## 고급 기능

### 이벤트 소싱 & 재개

```mermaid
flowchart LR
    Mission --> Events --> ES[(EventStore)]
    ES --> Replay[과거 재현]
    ES --> Resume[중단점 재개]
    ES --> Audit[이력 추적]
```

**사용 예시:**
```bash
# 중단된 미션 재개
claude-pilot resume mission-123

# 실패한 미션 재시도
claude-pilot retry mission-123

# 특정 체크포인트에서 재개
claude-pilot resume mission-123 --checkpoint cp-456
```

### P2P 충돌 해결

```mermaid
sequenceDiagram
    participant A as Coder A
    participant FOM as FileOwnership
    participant B as Coder B

    A->>FOM: file.rs 소유권 요청
    FOM-->>A: 소유권 획득
    B->>FOM: file.rs 소유권 요청
    FOM-->>B: 대기 (DeferredQueue)
    A->>FOM: 작업 완료, 해제
    FOM-->>B: 소유권 획득
    B->>B: 자동 재시도
```

### 수렴적 검증 (2-Pass)

```mermaid
flowchart TD
    R1[Round 1] --> C1{클린?}
    C1 -->|No| Fix1[자동 수정] --> R1
    C1 -->|Yes| R2[Round 2<br/>Clean #1]
    R2 --> C2{클린?}
    C2 -->|No| Fix2[자동 수정] --> R1
    C2 -->|Yes| Done[2회 연속 클린<br/>수학적 수렴 달성!]
```

---

## 실제 사용 사례

### 신규 기능 개발

```bash
claude-pilot mission "OAuth2.0 소셜 로그인 구현" --isolated
claude-pilot status
claude-pilot merge --pr
```

### 긴급 버그 수정

```bash
claude-pilot mission "결제 실패 롤백 누락 수정" --priority P1 --direct
claude-pilot merge --direct
```

### 대규모 리팩토링

```bash
claude-pilot mission "인증 시스템 JWT→Session 전환" \
  --isolated --on-complete pr

# 실시간 상태 확인
claude-pilot -o stream status
```

### 중단된 미션 재개

```bash
claude-pilot list
claude-pilot resume mission-2024-01-15-abc123
```

---

## 문제 해결

### 자주 발생하는 문제

#### 미션 타임아웃

```toml
[multi_agent.consensus]
total_timeout_secs = 3600  # 1시간으로 증가

[orchestrator]
mission_timeout_secs = 86400  # 1일로 증가
```

#### 합의 미수렴

```toml
[multi_agent.consensus]
max_rounds = 10
enable_cross_visibility = true  # 반드시 활성화
```

#### 검증 무한 루프

```toml
[recovery.convergent_verification]
max_rounds = 5
max_fix_attempts_per_issue = 3
```

#### Worktree 정리

```bash
claude-pilot cleanup mission-123
claude-pilot cleanup --all
```

### 디버깅

```bash
# 상세 로그
claude-pilot -v mission "..."

# 더 상세한 로그
RUST_LOG=debug claude-pilot mission "..."

# JSON 상태 확인
claude-pilot -o json status mission-123

# 이벤트 로그 조회
sqlite3 .pilot/events.db "SELECT * FROM events ORDER BY timestamp DESC LIMIT 20;"
```

---

## 개발

### 빌드 & 테스트

```bash
cargo build --release
cargo test --lib
cargo clippy
```

### 프로젝트 구조

```
claude-pilot/
├── src/
│   ├── agent/multi/      # 멀티 에이전트 핵심
│   ├── orchestration/    # 미션 오케스트레이션
│   ├── state/            # 이벤트 소싱
│   └── verification/     # 검증 시스템
├── tests/                # 통합 테스트
├── CLAUDE.md             # AI 개발 가이드
└── README.md             # 사용자 가이드
```

---

## 라이선스

MIT License

---

## 버전

- Rust Edition: 2024
- MSRV: 1.92.0
