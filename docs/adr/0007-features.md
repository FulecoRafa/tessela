# 0007 — Tudo opcional atrás de features

## Decisão
Cada engine, módulo e renderer é um crate próprio, exposto pela fachada
`tessela` atrás de uma feature. O padrão é `luau, quickjs, unix, tui`; `gpui`
é opt-in (pesado, exige bibliotecas de desktop). O workspace usa
`default-members` para que `cargo build`/`cargo test` não compilem o GPUI.

## Consequências
- Um app de mensageria pode compilar só `luau` e nenhum módulo de rede.
- O CI precisa testar a matriz de features; a fase 0 verificou
  `none`, `luau`, `quickjs+unix`, `luau+tui`, padrão e `gpui`.
