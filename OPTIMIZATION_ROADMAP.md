# Charm 최적화 로드맵

## 목표: Windsurf Cascade + OPENCODE급 성능 달성

### Phase 1: 토큰 관리 고도화 (레이턴시 30% 감소 예상)

#### 1.1 정확한 토큰 카운팅
- [ ] `tiktoken-rs` 또는 `tokenizers` 통합
- [ ] 모델별 토크나이저 선택 (cl100k_base, o200k_base)
- [ ] 실시간 컨텍스트 윈도우 모니터링

```rust
// 개선 방향
pub struct TokenCounter {
    tokenizer: Tokenizer,  // tiktoken
}

impl TokenCounter {
    pub fn count(&self, text: &str) -> usize {
        self.tokenizer.encode(text, true).len()
    }
    
    pub fn count_messages(&self, msgs: &[Message]) -> usize {
        // OpenAI 형식의 정확한 토큰 계산
    }
}
```

#### 1.2 스마트 컨텍스트 압축
- [ ] 중요도 기반 메시지 순위화
- [ ] 요약이 아닌 "키 포인트 추출"
- [ ] 코드 스니펫은 보존, 중간 대화는 압축

```rust
pub enum CompressionStrategy {
    PreserveCode,      // 코드 블록은 항상 유지
    SummarizeText,     // 일반 텍스트는 요약
    DropOldToolOutput, // 오래된 tool 결과 제거
}
```

### Phase 2: 캐싱 및 인덱싱 (레이턴시 40% 감소 예상)

#### 2.1 도구 스키마 캐싱
```rust
pub struct CachedToolSchemas {
    schemas: OnceCell<Vec<ToolSchema>>,
    json_strings: OnceCell<Vec<String>>,  // 미리 직렬화
}
```

#### 2.2 임베딩 기반 시맨틱 검색
- [ ] `fastembed-rs` 통합 (로컬 임베딩)
- [ ] FAISS 인덱스 빌드
- [ ] 증분 인덱싱 (변경된 파일만 재처리)

```rust
pub struct SemanticIndex {
    model: FastEmbedModel,
    faiss_index: IndexFlatIP,  // 내적 기반 검색
    file_hashes: HashMap<PathBuf, u64>,  // 변경 감지
}
```

#### 2.3 코드 인덱스 메모리 매핑
- [ ] `memmap2` 활용한 대형 인덱스 로딩
- [ ] 백그라운드 인덱싱 워커

### Phase 3: 스트리밍 및 동시성

#### 3.1 스트리밍 지원
```rust
pub enum ChatResponse {
    Streaming(Stream<Item=TokenChunk>),
    Blocking(Message),
}
```

#### 3.2 병렬 도구 실행
```rust
// 독립적인 도구 호출은 동시 실행
let futures = tool_calls.iter().map(|call| {
    tokio::spawn(execute_tool(call))
});
let results = join_all(futures).await;
```

### Phase 4: RTK (Retrieval Toolkit) 고도화

현재 `rtk_filter.rs`는 있지만 미사용 중. OPENCODE급 기능 추가:

```rust
pub struct RTKQuery {
    pub intent: QueryIntent,  // explore, plan, implement, verify
    pub depth: SearchDepth,   // quick, deep, exhaustive
    pub context_budget: usize, // 토큰 예산
}

pub async fn intelligent_retrieval(
    query: &RTKQuery,
    codebase: &CodebaseIndex,
) -> RetrievalResult {
    // 질의 의도에 따른 검색 전략 선택
    match query.intent {
        Explore => parallel_search_with_ranking(query),
        Plan => dependency_graph_traversal(query),
        Implement => precise_symbol_lookup(query),
        Verify => test_and_example_search(query),
    }
}
```

### Phase 5: 메모리 시스템 강화

#### 5.1 계층적 메모리
```rust
pub struct HierarchicalMemory {
    working_memory: Vec<Turn>,      // 현재 세션
    short_term: Vec<SessionSummary>, // 최근 세션
    long_term: EmbeddedMemories,    // 벡터 DB 저장
}
```

#### 5.2 자동 메모리 커밋
- [ ] 중요한 도구 실행 결과 자동 기록
- [ ] 세션 종료 시 요약 자동 생성

## 구현 우선순위

### 즉시 구현 (High Impact, Low Effort)
1. 토큰 카운터 정확화 (`tiktoken`)
2. 도구 스키마 캐싱
3. 컨텍스트 압축 개선

### 단기 구현 (1-2주)
4. 임베딩 기반 시맨틱 검색
5. 스트리밍 지원
6. 병렬 도구 실행

### 중기 구현 (2-4주)
7. RTK 고도화
8. 계층적 메모리 시스템
9. 백그라운드 인덱싱

## 성능 목표

| 지표 | 현재 | 목표 | 개선률 |
|------|------|------|--------|
| 첫 응답 시간 | 2-3s | <1s | 60%↓ |
| 컨텍스트 압축 정확도 | ~70% | >95% | 35%↑ |
| 시맨틱 검색 정확도 | ~50% | >85% | 70%↑ |
| 대형 코드베이스 인덱싱 | 30s+ | <5s | 80%↓ |
| 메모리 사용량 | 높음 | 최적화 | 40%↓ |
