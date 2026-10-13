# 0020 — Painéis sobre a barra e controles contínuos

## Contexto
Os painéis da shell de referência (central de controle, notificações, IA,
energia) pendem da barra e fecham com um clique fora, mas um clique *na
barra* não os fecha: o próprio botão decide se alterna ou troca de painel.
Aqui, uma superfície com `onDismiss` é um overlay do tamanho da saída, e o
fundo dele engolia o clique na barra. Os painéis também pediram o que só
cliques não dão: arrastar volume e brilho, listas que rolam até uma altura,
texto que quebra em linhas, campo de senha.

## Decisão
- A região de entrada de um overlay é o conteúdo mais a saída **menos o
  espaço que as outras superfícies do projeto reservam** (`exclusive`, como
  declarado, também sob `--dev`). Cliques nessa faixa chegam à barra.
- `Slider` no vocabulário estilizado: `value` 0..1, `onChange({ value })`,
  cor, trilho, espessura, `step` da roda. O renderer move o nível sob o
  ponteiro durante o arrasto e reporta só percentuais inteiros; o guest
  devolve o `value`. Sem `onChange` é um medidor. Fallback: `Progress`.
- `Box.maxHeight` (com `scroll`, uma lista que cresce e depois rola),
  `Label.wrap`/`lines` (quebra e reticências após N linhas), `Input.secret`
  (pontos; o fallback também nunca mostra o texto).
- O clique vai para a caixa clicável mais interna: um botão de fechar num
  cartão clicável não clica também o cartão.

## Consequências
- Painéis trocam e fecham pela barra como na referência, sem o protocolo de
  focus grab do Hyprland.
- Arrastar custa uma entrega por ponto percentual, não por movimento.
- Uma lista grande renderizada de uma vez ainda pode estourar o orçamento de
  CPU do guest (50 ms): a central limita as redes listadas.
