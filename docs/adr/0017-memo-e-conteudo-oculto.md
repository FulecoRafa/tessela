# 0017 — `memo()` e conteúdo oculto não é avaliado

## Contexto
O devtools mostrou a barra recriando todos os tiles de apps a cada segundo:
o binding lia a lista de janelas inteira, e títulos mudam o tempo todo. E a
gaveta, oculta, re-renderizava 166 bindings a cada leitura de CPU.

## Decisão
`memo(fn)` (Luau e JS) é um valor derivado, como o `createMemo` do Solid:
`fn` re-executa quando o que leu muda, e quem lê o memo só re-executa se o
resultado for diferente. O core guarda o resultado num sinal; a instância
reavalia memos sujos antes de cada passada de render.

Um `Surface` com `visible=false` e um `Popup` com `open=false` não
materializam os filhos; os bindings deles mantêm o cache para quando
reaparecerem.

## Consequências
- Derivar dados grossos em valores estáveis é explícito e barato.
- Uma superfície oculta custa só o binding de `visible`; um menu de bandeja
  só é lido quando abre.
- Reabrir algo oculto recria seus nós (ids novos).
