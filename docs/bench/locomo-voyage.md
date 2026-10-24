# LoCoMo retrieval bench

provider=voyage model=voyage-4-large rerank=true pool=50 mmr=false

### scale 1x (one store per conversation)

memories=5882 dedup-rejected=0 ingest=1739s recall p50=636ms p95=1241ms tokens@10 mean=435

| category | n | hit@1 | hit@5 | hit@10 | hit@25 | evidence recall@10 | MRR |
|---|---|---|---|---|---|---|---|
| multi-hop | 282 | 54.3 | 68.8 | 72.0 | 73.0 | 44.4 | 0.614 |
| temporal | 321 | 67.6 | 79.4 | 80.1 | 80.4 | 77.4 | 0.726 |
| open-domain | 92 | 26.1 | 44.6 | 48.9 | 52.2 | 37.8 | 0.335 |
| single-hop | 841 | 65.3 | 76.6 | 77.3 | 77.8 | 76.2 | 0.703 |
| **all** | 1536 | 61.4 | 73.8 | 75.2 | 75.9 | 68.3 | 0.669 |

