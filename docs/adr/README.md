# Decisões de arquitetura (ADRs)

Uma decisão por arquivo: contexto, decisão, consequências. Mudou de ideia?
Escreva um ADR novo que substitui o antigo; não reescreva o histórico.

| # | Decisão | Estado |
|---|---|---|
| [0001](0001-core-sem-io.md) | O core não faz I/O; tudo passa por módulos | aceita |
| [0002](0002-reatividade-no-host.md) | Reatividade mora no host, não no guest | aceita |
| [0003](0003-vocabulario-minimo-de-ui.md) | Vocabulário fechado, mínimo e voltado a UI | aceita |
| [0004](0004-capacidades.md) | Capacidades `tipo:recurso` declaradas no manifesto | aceita |
| [0005](0005-renderer-como-contrato.md) | Renderer é um trait; o modo remoto é previsto | aceita |
| [0006](0006-pontos-de-extensao-sob-demanda.md) | Pontos de extensão entram só com um app que precise | aceita |
| [0007](0007-features.md) | Toda peça opcional fica atrás de uma feature | aceita |
| [0008](0008-nome-provisorio.md) | Nome provisório "tessela"; licença MIT OR Apache-2.0 | aceita |
| [0009](0009-mensageria-interna.md) | Mensageria mira rede interna, não substituir email público | aceita |
