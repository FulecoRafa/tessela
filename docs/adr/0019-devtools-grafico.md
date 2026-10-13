# 0019 — Devtools gráfico, destaque e seleção na shell

## Contexto
O devtools de terminal ([0018](0018-devtools.md)) responde perguntas, mas
achar o nó de um ícone na árvore, ver o que mudou num frame ou editar um
valor ainda pedia comandos e ids. O DevTools de um navegador resolve isso
apontando: passar o mouse num nó acende o elemento na página, e clicar na
página seleciona o nó.

## Decisão
- `tessela devtools ui [--project NOME]` abre uma janela GPUI comum (xdg
  toplevel, não layer-shell) no mesmo binário: reaproveita a descoberta do
  socket e o protocolo do cliente, e o binário já carrega o GPUI. Abas
  Elements, Console, Network, Performance e Application; uma thread segue os
  canais (`watch`) e outra consulta o resto, reconectando sozinhas.
- O protocolo ganha `node` (props, handlers, caminho e onde foi desenhado),
  `highlight` (contornar um nó, ou limpar), `pick` e o canal `picks`.
- O renderer recebe pedidos por um `InspectControl` que entrega ao probe.
  Só enquanto há algo a contornar ele mede os nós: cada nó vira um elemento
  que adota o layout do filho e anota seus limites, sem mudar nada na tela.
  Por cima do conteúdo, uma camada desenha o contorno com `Tipo #id l×a`.
- No modo de seleção essa camada recebe todos os eventos de ponteiro:
  mover acende o nó mais interno sob o cursor, o clique o seleciona (e
  nenhum handler roda), o botão direito cancela.
- `log` informa de onde foi chamado: Luau pela pilha, JS pelo `Error().stack`
  com a linha mapeada de volta ao TSX pelo source map do transpile.

## Consequências
- O devtools gráfico é só mais um cliente: o CLI continua igual e o socket
  continua `0600`.
- Medir custa um elemento a mais por nó, e só enquanto se inspeciona.
- Uma superfície que cobre a tela (gaveta com `onDismiss`) também cobre a
  janela do devtools no mesmo compositor; com `--nested` ela fica em outro.
- Sem renderer gráfico (`--headless`), `highlight` e `pick` respondem erro.
