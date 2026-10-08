# 0002 — Reatividade no host

## Contexto
Luau e JS têm modelos de objeto diferentes. Implementar signals em cada um
geraria semânticas sutilmente diferentes.

## Decisão
Signals, bindings e ownership vivem em Rust (`tessela-core::reactive`). O
guest só chama `get/set/peek` e registra funções. Componentes rodam uma vez
(estilo Solid); o que é reativo é função passada como prop. Retornar uma
função como raiz dá o estilo React (o widget inteiro vira um binding).

## Consequências
- Todo engine novo (WASM, por exemplo) herda a mesma semântica de graça.
- Vazamentos são testáveis em um só lugar (`rebuilding_lists_does_not_leak`).
- Custo: cada leitura de signal cruza a fronteira do engine. Medido: irrelevante
  para UI de shell (0,5 % de CPU a 100 Hz no benchmark).
