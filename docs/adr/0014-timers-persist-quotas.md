# 0014 — Timers, persist e quotas

## Timers
Módulo `timer` (crate `tessela-timer`, capacidade `timer`): `after(ms, fn)` e
`every(ms, fn)`, ambos devolvendo uma função que cancela. O timer pertence ao
código que o criou: um `every` dentro de um binding para quando o binding
re-executa. `every` tem intervalo mínimo de 16 ms. Uma thread serve o
processo todo.

## Persist
`persist(key, initial)` é um `signal` cujo valor sobrevive a hot reloads da
extensão (o host guarda um `PersistStore` por slot). Ainda não sobrevive a
reiniciar o processo; isso pede um módulo de storage.

## Quotas
Por extensão, configuráveis no manifesto:
- `calls_per_sec` (padrão 200): balde de fichas sobre chamadas a módulos;
- `max_resources` (padrão 256): assinaturas, timers e afins vivos ao mesmo tempo.

Recursos que terminam sozinhos (um `after` que disparou) avisam o grafo com
`Inbound::Finished` e deixam de contar.
