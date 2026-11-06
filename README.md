# ⚡ aitop (AI Top)

> **Real-time Token & Cost Monitor for Claude Code, Codex, and Antigravity (AGY)**  
> 24시간 띄워두고 사용하는 초경량·고성능 Rust 기반 터미널 사용량 대시보드

---

## ✨ Features

- **3대 AI 코딩 에이전트 통합 모니터링**:
  - 🟣 **Claude Code** (Anthropic)
  - 🟢 **Codex CLI** (OpenAI)
  - 🔵 **Antigravity CLI (AGY)** (Google DeepMind)
- **실시간 TUI 대시보드 (`Ratatui`)**:
  - CPU 0.0%, RAM 8MB 미만의 극도로 가벼운 리소스 소모
  - 세션 수, 입력 토큰, 출력 토큰, 캐시 토큰, 총 토큰, 추정 비용 실시간 표시
- **주기 설정 (임의 조절 가능)**:
  - 기본 3분 (180초), 사용자가 원하는 초 단위 설정 가능 (`-i 300`)
- **키보드 단축키 지원**:
  - `q` / `Esc`: 종료
  - `r`: 즉시 새로고침
  - `s`: 현재 통계를 `aitop_usage_history.csv`로 저장
  - `Space`: 자동 새로고침 일시정지 / 재개
- **One-shot CLI 모드 (`--once`)**:
  - TUI를 띄우지 않고 쉘 스크립트나 터미널에서 즉시 1회 요약 출력

---

## 🚀 빠른 시작

```bash
# 기본 실행 (3분 주기 자동 갱신 TUI)
aitop

# 5분(300초) 주기로 실행
aitop -i 300

# 10초 주기로 빠르게 모니터링
aitop -i 10

# TUI 없이 현재 사용량 즉시 요약 출력
aitop --once

# 현재 통계를 CSV 파일로 저장
aitop --csv my_usage.csv
```

---

## 📂 프로젝트 구조

```text
aitop/
├── Cargo.toml
├── README.md
└── src/
    ├── main.rs        # CLI 파싱, 터미널 루프, 키 이벤트 처리
    ├── models.rs      # 데이터 모델 및 포맷팅 헬퍼
    ├── ui.rs          # Ratatui TUI 레이아웃 및 렌더링
    ├── export.rs      # CSV 아카이빙 및 저장 로직
    └── parser/        # 에이전트별 로컬 로그 파서
        ├── mod.rs
        ├── claude.rs  # ~/.claude/stats-cache.json 파서
        ├── codex.rs   # ~/.codex/sessions/**/*.jsonl 파서
        └── agy.rs     # ~/.gemini/antigravity-cli/brain/**/*.jsonl 파서
```
