# 0005 — Renderer é um contrato

## Decisão
`trait Renderer { fn run(self, Surface) }`. O renderer recebe um `Surface`
(slots + stream de `Frame`) e devolve `UiEvent`s. Ele toma a thread que o
chama, porque toolkits gráficos exigem a thread principal.

Na fase 0 cada `Frame` carrega a árvore inteira. Como a árvore é fechada e
serializável, a fronteira runtime↔renderer pode virar protocolo; a fase 1
introduz ids estáveis e patches já pensando nisso, e a fase 3 implementa o
modo remoto (guest no servidor, celular só renderiza).

## Consequências
- GPUI fica isolado em `tessela-gpui`; se a API dele quebrar, nada mais quebra.
- O `Headless` serve de renderer de testes e benchmarks.
