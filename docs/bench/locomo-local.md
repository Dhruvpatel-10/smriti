# LoCoMo retrieval bench

provider=local model=bge-small-en-v1.5 rerank=false pool=50 mmr=false

### scale 1x (one store per conversation)

memories=5882 dedup-rejected=0 ingest=389s recall p50=102ms p95=114ms tokens@10 mean=380

| category | n | hit@1 | hit@5 | hit@10 | hit@25 | evidence recall@10 | MRR |
|---|---|---|---|---|---|---|---|
| multi-hop | 282 | 21.3 | 52.8 | 65.6 | 78.4 | 38.1 | 0.360 |
| temporal | 321 | 43.6 | 68.8 | 76.0 | 81.9 | 72.7 | 0.544 |
| open-domain | 92 | 16.3 | 37.0 | 48.9 | 59.8 | 37.3 | 0.257 |
| single-hop | 841 | 36.4 | 63.3 | 72.4 | 82.4 | 71.1 | 0.485 |
| **all** | 1536 | 33.9 | 60.9 | 70.5 | 80.2 | 63.3 | 0.461 |

### scale 10x (all conversations pooled)

memories=5882 dedup-rejected=0 ingest=1518s recall p50=705ms p95=794ms tokens@10 mean=355

| category | n | hit@1 | hit@5 | hit@10 | hit@25 | evidence recall@10 | MRR |
|---|---|---|---|---|---|---|---|
| multi-hop | 282 | 18.1 | 48.2 | 62.1 | 75.9 | 35.0 | 0.317 |
| temporal | 321 | 40.8 | 64.2 | 72.0 | 81.9 | 68.5 | 0.512 |
| open-domain | 92 | 13.0 | 30.4 | 41.3 | 55.4 | 32.0 | 0.228 |
| single-hop | 841 | 35.4 | 59.1 | 70.7 | 80.9 | 69.3 | 0.467 |
| **all** | 1536 | 32.0 | 56.4 | 67.6 | 78.6 | 60.6 | 0.435 |


### 10x pooled with HNSW index (B1)

memories=5882 ingest=1699s recall p50=471ms p95=539ms (brute force: p50 705ms)

| category | n | hit@1 | hit@5 | hit@10 | hit@25 | evidence recall@10 | MRR |
|---|---|---|---|---|---|---|---|
| multi-hop | 282 | 17.0 | 48.2 | 61.3 | 74.8 | 34.0 | 0.311 |
| temporal | 321 | 39.3 | 63.2 | 71.3 | 81.0 | 68.0 | 0.500 |
| open-domain | 92 | 13.0 | 29.3 | 40.2 | 54.3 | 30.0 | 0.221 |
| single-hop | 841 | 34.4 | 57.0 | 68.8 | 79.8 | 67.5 | 0.453 |
| **all** | 1536 | 30.9 | 55.0 | 66.3 | 77.6 | 59.2 | 0.423 |
