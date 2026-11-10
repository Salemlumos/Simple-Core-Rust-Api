| Cenário | Taxa pedida | RPS | p50 ms | p95 ms | p99 ms | Erros | CPU API méd/máx | Mem API máx | CPU PG méd/máx | Mem PG máx | Conexões runtime méd/máx | Acerto dos caches |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| get_by_id | 15000 | 14994 | 0.40 | 0.97 | 7.25 | 0.000% | 282% / 299% | 69 MiB | 178% / 191% | 482 MiB | 12 / 12 (ativas máx 3) | tenant_pool 100.00% tenant_registry 100.00% access 99.78%  |
| list | 4000 | 4000 | 0.49 | 0.64 | 0.75 | 0.000% | 98% / 112% | 72 MiB | 71% / 81% | 605 MiB | 12 / 12 (ativas máx 2) | tenant_pool 100.00% tenant_registry 100.00% access 99.17%  |
| mix_single | 9000 | 8989 | 0.44 | 2.43 | 14.01 | 0.000% | 211% / 235% | 72 MiB | 142% / 161% | 710 MiB | 20 / 20 (ativas máx 5) | tenant_pool 100.00% tenant_registry 100.00% access 99.63%  |
| mix_all | 9000 | 9000 | 0.46 | 2.08 | 3.07 | 0.000% | 213% / 225% | 103 MiB | 161% / 189% | 2862 MiB | 688 / 691 (ativas máx 6) | tenant_pool 100.00% tenant_registry 99.96% access 97.72%  |
| pool_churn | 8000 | 947 | 765.89 | 1532.24 | 1592.33 | 0.060% | 191% / 201% | 157 MiB | 382% / 389% | 1408 MiB | 137 / 237 (ativas máx 3) | tenant_pool 41.13% tenant_registry 99.65% access 84.39%  |
| noisy_neighbor (normais) | 1500 | 1500 | 0.44 | 0.76 | 1.18 | 0.000% | 163% / 196% | 115 MiB | 98% / 120% | 2715 MiB | 724 / 724 (ativas máx 1) | tenant_pool 100.00% tenant_registry 99.96% access 97.88%  |
| noisy_neighbor (barulhento, 6000/s) | 1500 | 6000 | 0.40 | 0.57 | 0.95 | 0.000% | 163% / 196% | 115 MiB | 98% / 120% | 2715 MiB | 724 / 724 (ativas máx 1) | tenant_pool 100.00% tenant_registry 99.96% access 97.88%  |
| login | 150 | 150 | 11.15 | 13.73 | 16.02 | 0.000% | 160% / 166% | 186 MiB | 10% / 21% | 2740 MiB | 238 / 729 (ativas máx 1) | tenant_pool 100.00% tenant_registry 99.98%  |
| create | 150 | 150 | 12.28 | 34.99 | 389.73 | 0.326% | 164% / 182% | 190 MiB | 10% / 13% | 950 MiB | 20 / 20 (ativas máx 10) | tenant_pool 100.00% tenant_registry 99.98% access 81.22%  |
| read_with_invalidations | 10000 | 9999 | 0.43 | 0.80 | 2.47 | 0.000% | 224% / 241% | 111 MiB | 149% / 157% | 954 MiB | 20 / 20 (ativas máx 3) | tenant_pool 100.00% tenant_registry 100.00% platform 98.34% access 80.17%  |
| read_with_provisioning | 10000 | 10000 | 0.39 | 0.58 | 1.48 | 0.000% | 214% / 228% | 115 MiB | 129% / 144% | 1154 MiB | 20 / 20 (ativas máx 2) | tenant_pool 100.00% tenant_registry 100.00% access 99.67%  |
