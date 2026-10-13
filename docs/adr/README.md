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
| [0010](0010-vocabulario-com-schemas.md) | Nós genéricos validados por schema; componentes de app com fallback | aceita |
| [0011](0011-patches.md) | Frames são patches com ids estáveis | aceita |
| [0012](0012-acoes-declarativas.md) | Props de evento aceitam ações executadas pelo host | aceita |
| [0013](0013-pool-de-workers.md) | Pool de workers em vez de uma thread por extensão | aceita |
| [0014](0014-timers-persist-quotas.md) | Timers, `persist` e quotas por extensão | aceita |
| [0015](0015-projetos.md) | Projetos: entradas, pacotes `@nome`, superfícies, primitivos estilizados, temas | aceita |
| [0016](0016-dados-da-extensao.md) | Sinais de módulo pertencem à extensão; estado compartilhado e módulos de desktop | aceita |
