# Claude-Pilot

> AI 코딩 오케스트레이터 - 증거 기반 계획, 멀티 에이전트 합의, 수렴적 품질 검증으로 완벽한 코드를 만듭니다.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         왜 Claude-Pilot인가?                                 │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   기존 AI 코딩                          Claude-Pilot                        │
│   ────────────────                      ─────────────                       │
│   ❌ 추측으로 코딩                      ✅ 증거 수집 후 계획                 │
│   ❌ "완료!" 후 버그 발견               ✅ 2회 연속 검증 통과까지 반복       │
│   ❌ 복잡한 작업에서 일관성 부족        ✅ 멀티 에이전트 합의로 조율         │
│   ❌ 중단되면 처음부터 다시             ✅ 이벤트 소싱으로 중단점에서 재개   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

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
- [성능 튜닝](#성능-튜닝)
- [문제 해결](#문제-해결)

---

## 빠른 시작

### 요구사항

- Rust 1.92.0 이상
- Git
- Claude Code CLI 설치 및 OAuth 인증 완료

### 설치

```bash
git clone https://github.com/your-org/claude-pilot.git
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

### 격리 모드 선택

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

### 아키텍처 개요

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        Claude-Pilot 아키텍처                                 │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│                          ┌─────────────────┐                                │
│                          │ MissionOrchestrator│                             │
│                          │ (미션 생명주기)    │                              │
│                          └────────┬────────┘                                │
│                                   │                                         │
│          ┌────────────────────────┼────────────────────────┐               │
│          ▼                        ▼                        ▼               │
│  ┌──────────────┐        ┌──────────────┐        ┌──────────────┐          │
│  │  AgentPool   │        │ Coordinator  │        │  EventStore  │          │
│  │ (에이전트 풀)│◀──────▶│ (조율/합의)   │──────▶│ (이벤트 저장) │          │
│  └──────────────┘        └──────────────┘        └──────────────┘          │
│          │                        │                        │               │
│          ▼                        ▼                        ▼               │
│  ┌──────────────┐        ┌──────────────┐        ┌──────────────┐          │
│  │ Research     │        │ Consensus    │        │ Replay/      │          │
│  │ Planning     │        │ Engine       │        │ Resume       │          │
│  │ Coder        │        │ (합의 엔진)   │        │ (재개 지원)   │          │
│  │ Verifier     │        └──────────────┘        └──────────────┘          │
│  │ Reviewer     │                                                          │
│  └──────────────┘                                                          │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 3가지 핵심 보장

| 보장 | 설명 | 구현 방식 |
|------|------|-----------|
| **품질 보장** | 2회 연속 검증 통과 필수 | ConvergentVerifier |
| **일관성 보장** | 멀티 에이전트 합의 | Cross-Visibility Consensus |
| **내구성 보장** | 중단점에서 재개 가능 | Event Sourcing (SQLite) |

---

## 실행 흐름

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                              미션 실행 단계                                   │
└──────────────────────────────────────────────────────────────────────────────┘

  "사용자 인증 기능 추가"
         │
         ▼
┌─────────────────┐
│   Phase 1       │  ResearchAgent가 코드베이스 분석
│   Research      │  • 기존 구조 파악
│                 │  • 의존성 확인
│                 │  • 증거 수집
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Phase 2       │  PlanningAgent들이 합의
│   Planning      │  • 작업 분해
│   (Consensus)   │  • Cross-visibility로 제안 공유
│                 │  • 일관된 계획 도출
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Phase 3       │  CoderAgent들이 구현
│   Implement     │  • 코드 생성
│                 │  • 파일 충돌 시 P2P 해결
│                 │  • FileOwnership으로 동기화
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Phase 4       │  VerifierAgent + ReviewerAgent
│   Verify        │  • Build/Test/Lint 검증
│   (Convergent)  │  • 코드 리뷰
│                 │  • 이슈 발견 시 자동 수정
└────────┬────────┘
         │
         ▼
    ┌─────────┐
    │ Clean?  │────NO────▶ Phase 3으로 돌아가서 수정
    └────┬────┘
         │YES
         ▼
    ┌─────────┐
    │ 2회     │────NO────▶ 한 번 더 검증
    │ 연속?   │
    └────┬────┘
         │YES
         ▼
    ┌─────────┐
    │  완료!  │
    └─────────┘
```

---

## CLI 명령어

### 미션 관리

| 명령어 | 설명 |
|--------|------|
| `claude-pilot init` | 프로젝트 초기화 (.pilot/, .claudegen/ 생성) |
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
| `claude-pilot extract [mission_id]` | 학습 패턴 추출 |

### 옵션

```bash
# 미션 시작 옵션
--isolated              # Worktree 격리 모드 (권장)
--branch                # 브랜치 격리 모드
--direct                # 현재 브랜치에서 작업
--priority <P1|P2|P3>   # 우선순위 (P1=긴급)
--on-complete <pr|manual|direct>  # 완료 시 액션

# 출력 형식
-o text                 # 텍스트 (기본값)
-o json                 # JSON
-o stream               # NDJSON 스트리밍 (실시간)

# 디버깅
-v, --verbose           # 상세 로그
```

---

## 설정

### 설정 파일 위치

```
your-project/
├── .pilot/
│   ├── config.toml       # 프로젝트 설정
│   └── events.db         # 이벤트 저장소 (SQLite)
└── .claudegen/
    └── manifest.json     # 모듈 구조 정의
```

### 핵심 설정 (.pilot/config.toml)

```toml
# ═══════════════════════════════════════════════════════════════
# 기본 설정
# ═══════════════════════════════════════════════════════════════
[orchestrator]
max_iterations = 100           # 최대 반복 횟수
mission_timeout_secs = 604800  # 미션 타임아웃 (7일)

# ═══════════════════════════════════════════════════════════════
# 멀티 에이전트 시스템
# ═══════════════════════════════════════════════════════════════
[multi_agent]
enabled = true                 # 멀티 에이전트 ON
dynamic_mode = true            # Manifest 기반 동적 구성
parallel_execution = true      # 병렬 실행

# 에이전트 인스턴스 수
[multi_agent.instances]
research = 1                   # ResearchAgent 수
planning = 3                   # PlanningAgent 수 (합의용)
coder = 2                      # CoderAgent 수 (병렬 구현)
verifier = 1                   # VerifierAgent 수

# ═══════════════════════════════════════════════════════════════
# 합의 설정
# ═══════════════════════════════════════════════════════════════
[multi_agent.consensus]
max_rounds = 5                 # 최대 합의 라운드
min_participants = 2           # 최소 참여자
total_timeout_secs = 1800      # 합의 타임아웃 (30분)
enable_cross_visibility = true # 에이전트 간 제안 공유

# 합의 전략 임계값
flat_threshold = 3             # 3명 이하 → 단일 라운드 합의
hierarchical_threshold = 10    # 10명 초과 → 계층형 합의

# ═══════════════════════════════════════════════════════════════
# 수렴적 검증 (품질 보장)
# ═══════════════════════════════════════════════════════════════
[recovery.convergent_verification]
required_clean_rounds = 2      # 2회 연속 클린 필수 (변경 불가)
max_rounds = 10                # 최대 검증 라운드
max_fix_attempts_per_issue = 5 # 이슈당 최대 수정 시도
include_ai_review = true       # AI 코드 리뷰 포함 (변경 불가)

# ═══════════════════════════════════════════════════════════════
# 이벤트 저장소
# ═══════════════════════════════════════════════════════════════
[state]
database_path = ".pilot/events.db"
retention_days = 30            # 이벤트 보관 기간
enable_snapshots = true        # 스냅샷 활성화
snapshot_interval_events = 100 # 100 이벤트마다 스냅샷

# ═══════════════════════════════════════════════════════════════
# 컨텍스트 압축 (대규모 코드베이스용)
# ═══════════════════════════════════════════════════════════════
[context.compaction]
enabled = true
compaction_threshold = 0.7     # 70% 사용 시 압축 트리거
```

### 모듈 구조 정의 (.claudegen/manifest.json)

```json
{
  "project": {
    "name": "my-project",
    "workspaces": [
      {
        "id": "main",
        "path": ".",
        "domains": ["backend", "frontend"]
      }
    ],
    "domains": [
      {
        "id": "backend",
        "group_ids": ["api", "database"]
      },
      {
        "id": "frontend",
        "group_ids": ["ui"]
      }
    ],
    "groups": [
      {
        "id": "api",
        "module_ids": ["auth", "users"],
        "domain_id": "backend"
      },
      {
        "id": "database",
        "module_ids": ["models", "migrations"],
        "domain_id": "backend"
      },
      {
        "id": "ui",
        "module_ids": ["components", "pages"],
        "domain_id": "frontend"
      }
    ],
    "modules": [
      {
        "id": "auth",
        "name": "인증 모듈",
        "paths": ["src/auth/"],
        "dependencies": ["models"],
        "responsibility": "사용자 인증 및 권한 관리"
      },
      {
        "id": "users",
        "name": "사용자 모듈",
        "paths": ["src/users/"],
        "dependencies": ["auth", "models"],
        "responsibility": "사용자 CRUD"
      },
      {
        "id": "models",
        "name": "모델 모듈",
        "paths": ["src/models/"],
        "dependencies": [],
        "responsibility": "데이터 모델 정의"
      }
    ]
  }
}
```

---

## 멀티 에이전트 시스템

### 에이전트 역할

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                            에이전트 역할 분담                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Phase 1: Research                    Phase 2: Planning                      │
│  ┌─────────────────────┐              ┌─────────────────────┐                │
│  │   ResearchAgent     │              │   PlanningAgent ×3  │                │
│  │                     │              │                     │                │
│  │   • 코드베이스 분석 │              │   • 합의로 계획 도출│                │
│  │   • 증거 수집       │              │   • 작업 분해       │                │
│  │   • 의존성 파악     │              │   • 제안 공유       │                │
│  └─────────────────────┘              └─────────────────────┘                │
│                                                                              │
│  Phase 3: Implement                   Phase 4: Verify                        │
│  ┌─────────────────────┐              ┌─────────────────────┐                │
│  │   CoderAgent ×2     │              │   VerifierAgent     │                │
│  │                     │              │   ReviewerAgent     │                │
│  │   • 병렬 구현       │              │                     │                │
│  │   • 충돌 자동 해결  │              │   • Build/Test/Lint │                │
│  │   • P2P 조율        │              │   • 코드 리뷰       │                │
│  └─────────────────────┘              └─────────────────────┘                │
│                                                                              │
│  전체 Phase: Advisory                                                        │
│  ┌─────────────────────┐              ┌─────────────────────┐                │
│  │   ArchitectAgent    │              │   ModuleAgent       │                │
│  │                     │              │                     │                │
│  │   • 설계 검증       │              │   • 모듈별 전문성   │                │
│  │   • 아키텍처 조언   │              │   • 스코프 강제     │                │
│  └─────────────────────┘              └─────────────────────┘                │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 계층형 합의

```
크로스-워크스페이스 시나리오:

┌─────────────────────────────────────────────────────────────────────────────┐
│                                                                             │
│  Project A                              Project B                           │
│  ├── Domain: Backend                    ├── Domain: API                     │
│  │   ├── Module: auth    ─┐             │   └── Module: gateway ─┐          │
│  │   ├── Module: user    ─┼─ Group 1    │                        │          │
│  │   └── Module: session ─┘             └── Domain: Data         │          │
│  │                                          └── Module: cache ───┘          │
│  │                                                               │          │
│  └───────────────────────────┬───────────────────────────────────┘          │
│                              │                                              │
│                              ▼                                              │
│                ┌─────────────────────────────┐                              │
│                │   계층형 합의 (Bottom-Up)   │                              │
│                ├─────────────────────────────┤                              │
│                │                             │                              │
│                │   Tier 0: Module            │  ← 모듈별 에이전트 합의      │
│                │           ↓                 │                              │
│                │   Tier 1: Group             │  ← 그룹 코디네이터 합의      │
│                │           ↓                 │                              │
│                │   Tier 2: Domain            │  ← 도메인 코디네이터 합의    │
│                │           ↓                 │                              │
│                │   Tier 3: Workspace         │  ← 워크스페이스 합의         │
│                │           ↓                 │                              │
│                │   Tier 4: CrossWorkspace    │  ← 전체 프로젝트 합의        │
│                │                             │                              │
│                └─────────────────────────────┘                              │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Cross-Visibility 합의

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         합의 방식 비교                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  기존 방식 (독립 투표):              Claude-Pilot (Cross-Visibility):       │
│  ─────────────────────               ──────────────────────────────         │
│                                                                             │
│  Agent A: 제안 A  ─┐                 Agent A: 제안 A ─┐                     │
│  Agent B: 제안 B  ─┼─▶ 다수결        Agent B: 제안 B ─┼─▶ 실시간 공유       │
│  Agent C: 제안 C  ─┘    투표         Agent C: 제안 C ─┘        │            │
│                         │                                      ▼            │
│                         ▼                              ┌──────────────┐     │
│                   불일치 가능                          │ 서로 제안 확인│     │
│                   (각자 다른 방향)                     │ 의미론적 조정 │     │
│                                                        │ 일관된 합의  │     │
│                                                        └──────────────┘     │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 고급 기능

### 이벤트 소싱 & 재개

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                           이벤트 소싱 시스템                                  │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Mission 실행                                                                │
│       │                                                                      │
│       ▼                                                                      │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                      Event Store (SQLite)                               │ │
│  │                                                                         │ │
│  │  MissionStarted → ConsensusStarted → TaskCompleted → VerifyRound → ... │ │
│  │                                                                         │ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
│       │                    │                    │                            │
│       ▼                    ▼                    ▼                            │
│  ┌─────────┐          ┌─────────┐          ┌─────────┐                       │
│  │ Replay  │          │ Resume  │          │ Audit   │                       │
│  │         │          │         │          │         │                       │
│  │ 과거    │          │ 중단점  │          │ 전체    │                       │
│  │ 상태    │          │ 에서    │          │ 이력    │                       │
│  │ 재현    │          │ 계속    │          │ 추적    │                       │
│  └─────────┘          └─────────┘          └─────────┘                       │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

**사용 예시:**
```bash
# 미션이 중단되었을 때 재개
claude-pilot resume mission-123

# 실패한 미션 재시도 (성공한 작업 유지)
claude-pilot retry mission-123

# 특정 체크포인트에서 재개
claude-pilot resume mission-123 --checkpoint cp-456
```

### P2P 충돌 해결

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                           P2P 충돌 해결                                       │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Coder A ──────────────────┬──────────────────── Coder B                     │
│       │                    │                         │                       │
│       ▼                    ▼                         ▼                       │
│  ┌─────────┐          ┌─────────┐             ┌─────────┐                    │
│  │ file.rs │    ←──   │ file.rs │   ──▶       │ file.rs │                    │
│  │ 수정 중 │          │ (충돌!) │             │ 수정 중 │                    │
│  └─────────┘          └─────────┘             └─────────┘                    │
│       │                    │                         │                       │
│       └────────────────────┼─────────────────────────┘                       │
│                            ▼                                                 │
│                  ┌──────────────────┐                                        │
│                  │ FileOwnership    │                                        │
│                  │ Manager          │                                        │
│                  └────────┬─────────┘                                        │
│                           │                                                  │
│                           ▼                                                  │
│  ┌───────────────────────────────────────────────────────────────────────┐   │
│  │                                                                       │   │
│  │  1. Coder A: 소유권 획득 (ID 기반 결정적 선택)                        │   │
│  │  2. Coder A: 작업 진행                                                │   │
│  │  3. Coder B: DeferredQueue에 대기 등록                                │   │
│  │  4. Coder A: 완료 후 소유권 해제                                      │   │
│  │  5. Coder B: 자동 재시도                                              │   │
│  │                                                                       │   │
│  └───────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 수렴적 검증

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                           수렴적 검증 (2-Pass)                                │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  목표: 2회 연속 클린 라운드 달성                                              │
│                                                                              │
│  Round 1: Build ✅ → Test ✅ → Lint ⚠️ (warning)                             │
│           │                                                                  │
│           ▼ 이슈 발견 → 자동 수정                                            │
│                                                                              │
│  Round 2: Build ✅ → Test ✅ → Lint ✅ (Clean #1)                             │
│           │                                                                  │
│           ▼ 1회 클린 → 한 번 더 검증                                         │
│                                                                              │
│  Round 3: Build ✅ → Test ✅ → Lint ✅ (Clean #2)                             │
│           │                                                                  │
│           ▼                                                                  │
│                                                                              │
│  ✅ 2회 연속 클린 → 수학적으로 수렴 → 미션 완료!                             │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## 실제 사용 사례

### 사례 1: 신규 기능 개발

```bash
# 복잡한 기능 개발 (합의 기반 계획)
claude-pilot mission "OAuth2.0 소셜 로그인 구현" --isolated

# 진행 상황 확인
claude-pilot status

# 완료 후 PR 생성
claude-pilot merge --pr
```

### 사례 2: 버그 수정

```bash
# 긴급 버그 수정 (빠른 처리)
claude-pilot mission "결제 실패 시 롤백 누락 버그 수정" --priority P1 --direct

# 수정 후 즉시 병합
claude-pilot merge --direct
```

### 사례 3: 대규모 리팩토링

```bash
# 크로스-모듈 리팩토링
claude-pilot mission "인증 시스템 JWT에서 Session 기반으로 전환" \
  --isolated \
  --on-complete pr

# 장시간 작업 시 상태 확인
claude-pilot -o stream status
```

### 사례 4: 중단된 미션 재개

```bash
# 미션 목록 확인
claude-pilot list

# 중단된 미션 재개 (이전 진행 상황 유지)
claude-pilot resume mission-2024-01-15-abc123
```

---

## 성능 튜닝

### 작업 규모별 권장 설정

#### 소규모 작업 (단일 파일, 간단한 수정)

```toml
[multi_agent]
enabled = false  # 단일 에이전트 모드

[recovery.convergent_verification]
max_rounds = 5
```

#### 중규모 작업 (여러 파일, 단일 모듈)

```toml
[multi_agent]
enabled = true
dynamic_mode = false  # 코어 에이전트만 사용

[multi_agent.instances]
planning = 2
coder = 1

[multi_agent.consensus]
flat_threshold = 5
```

#### 대규모 작업 (크로스-모듈, 복잡한 의존성)

```toml
[multi_agent]
enabled = true
dynamic_mode = true   # Manifest 기반 동적 에이전트
parallel_execution = true

[multi_agent.instances]
planning = 5
coder = 3

[multi_agent.consensus]
enable_cross_visibility = true
hierarchical_threshold = 8
```

### 리소스 최적화

```toml
# 컨텍스트 압축 (대규모 코드베이스)
[context.compaction]
enabled = true
compaction_threshold = 0.6  # 더 공격적인 압축

# 이벤트 정리 (장기 실행)
[state]
retention_days = 14  # 2주로 단축
snapshot_interval_events = 50  # 더 자주 스냅샷
```

---

## 문제 해결

### 자주 발생하는 문제

#### 미션이 타임아웃됩니다
```toml
# .pilot/config.toml
[multi_agent.consensus]
total_timeout_secs = 3600  # 1시간으로 증가

[orchestrator]
mission_timeout_secs = 86400  # 1일로 증가
```

#### 합의가 수렴하지 않습니다
```toml
[multi_agent.consensus]
max_rounds = 10            # 라운드 증가
min_participants = 1       # 단순 작업용 (합의 필요 없음)
enable_cross_visibility = true  # 반드시 활성화
```

#### 검증이 무한 루프입니다
```toml
[recovery.convergent_verification]
max_rounds = 5             # 라운드 제한
max_fix_attempts_per_issue = 3
```

#### Worktree 정리가 필요합니다
```bash
# 특정 미션 정리
claude-pilot cleanup mission-123

# 모든 Worktree 정리
claude-pilot cleanup --all
```

#### OAuth 인증 문제
```bash
# Claude Code CLI 재인증
claude auth login
```

### 디버깅

```bash
# 상세 로그 활성화
claude-pilot -v mission "..."

# 더 상세한 로그
RUST_LOG=debug claude-pilot mission "..."

# JSON 출력으로 상태 확인
claude-pilot -o json status mission-123

# 스트리밍으로 실시간 확인
claude-pilot -o stream mission "..."

# 이벤트 로그 조회
sqlite3 .pilot/events.db "SELECT * FROM events ORDER BY timestamp DESC LIMIT 20;"
```

### 로그 위치

```
.pilot/
├── logs/
│   ├── mission-{id}.log      # 미션별 로그
│   └── claude-pilot.log      # 전체 로그
└── events.db                 # 이벤트 저장소
```

---

## 개발

### 빌드 & 테스트

```bash
# 빌드
cargo build
cargo build --release

# 테스트
cargo test               # 유닛 테스트
cargo test --lib         # 라이브러리 테스트만
cargo clippy             # 린트

# E2E 테스트 (실제 LLM 호출)
cargo test --test real_e2e_multi_agent_tests -- --ignored --nocapture
cargo test --test cross_workspace_consensus_e2e -- --ignored --nocapture
```

### 프로젝트 구조

```
claude-pilot/
├── src/
│   ├── agent/              # 에이전트 시스템
│   │   ├── multi/          # 멀티 에이전트 핵심
│   │   │   ├── core/       # 에이전트 기본 타입
│   │   │   ├── shared/     # 공유 계약/타입
│   │   │   ├── context/    # 컨텍스트 조합
│   │   │   ├── messaging/  # P2P 메시징
│   │   │   ├── session/    # 세션 관리
│   │   │   ├── rules/      # 도메인 규칙 (WHAT)
│   │   │   └── skills/     # 작업 스킬 (HOW)
│   │   └── task_agent.rs   # Claude Code CLI 인터페이스
│   ├── orchestration/      # 미션 오케스트레이션
│   ├── state/              # 이벤트 소싱
│   ├── workspace/          # 워크스페이스 관리
│   ├── recovery/           # 복구 & 수렴적 검증
│   └── verification/       # 검증 시스템
├── tests/                  # 통합 테스트
├── CLAUDE.md               # AI 에이전트 개발 가이드 (영어)
└── README.md               # 사용자 가이드 (한글)
```

---

## 라이선스

MIT License

---

## 버전 정보

- Rust Edition: 2024
- MSRV: 1.92.0
