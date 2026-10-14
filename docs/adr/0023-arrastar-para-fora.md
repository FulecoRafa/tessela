# 0023 — Arrastar para fora: `Box drag` e a superfície que deixa o ponteiro passar

## Contexto
O farol lista arquivos; o usuário quer arrastar uma linha e soltá-la na
janela de baixo: um campo de upload do navegador, um gerenciador de
arquivos, um chat, um terminal. No Wayland isso é um `wl_data_device.start_drag`
com um `wl_data_source`, e quem decide onde cai é o compositor.

O GPUI já sabe arrastar para fora, mas só quando um arrasto interno
(`on_drag` + `external_drag_payload`) **sai da janela**: o ponteiro tem de
deixar a área da superfície. Uma superfície com `onDismiss` é desenhada como
overlay transparente cobrindo a saída ([ADR 0020](0020-paineis-e-controles-continuos.md)),
então o ponteiro nunca sai; e o GPUI só oferece `text/uri-list`.

Há um segundo problema: durante o arrasto, o alvo é a superfície sob o
ponteiro, e o overlay está em cima de tudo. Soltar cairia nele.

## Decisão
- **`Box` ganha `drag`** (um caminho absoluto ou uma lista), **`onDragStart`**
  (sem payload) e **`onDragEnd({ dropped })`**. Pressionar o box e mover mais
  de 6 px começa o arrasto; soltar antes disso continua sendo clique
  (`onClick` no mesmo box funciona).
- O renderer reconhece o gesto e começa o arrasto ele mesmo, por uma função
  nova no crate de plataforma vendorizado: `gpui_linux::start_drag(window,
  offers, on_end)` (patch marcado `tessela patch`, listado em
  `vendor/README.md`). Ela usa o serial do botão ainda pressionado, a
  superfície da janela como origem e oferece pares tipo→bytes; o tessela
  oferece `text/uri-list` (URIs `file://` com percent-encoding, terminadas em
  CRLF) e `text/plain` (os caminhos, um por linha), só a ação *copy* — um
  launcher não deve deixar um gerenciador de arquivos *mover* o arquivo.
  `dropped` é `dnd_finished`; `cancelled`, mesmo depois de
  `dnd_drop_performed`, é `false`. O arrasto promovido do próprio GPUI passa
  pelo mesmo código. Enquanto um arrasto nosso está no ar, a própria janela
  de origem não aceita a oferta.
- **Passar através**: ao começar, a janela de origem recebe uma região de
  entrada vazia (`wl_surface.set_input_region` com região sem retângulos),
  e o compositor escolhe o alvo entre as superfícies de baixo. Ao terminar,
  a região volta (overlay: no quadro seguinte, pelo mesmo canvas que a
  calcula; superfície comum: região inteira). A superfície continua mapeada.
- Esconder a superfície durante o arrasto também funciona e é o que o farol
  fará em `onDragStart`: nem o wlroots (`types/data_device/wlr_drag.c`: o
  arrasto escuta a destruição da fonte, do ícone e da superfície em foco,
  não da origem) nem o Hyprland (`protocols/core/DataDevice.cpp`:
  `originSurface` é só uma referência fraca; aborta se o *ícone* sumir)
  cancelam o arrasto quando a origem some. O `onDragEnd` chega mesmo com a
  janela já fechada. A região vazia fica como garantia que não depende de o
  guest esconder nada.

## Consequências
- Testado no `--nested` (sway headless) com um ponteiro virtual
  (`zwlr_virtual_pointer_v1`) e um `foot` por baixo do overlay: o `foot`
  recebe o caminho (`'…/a file ç.txt'`, URI decodificada), `onDragEnd`
  chega com `dropped: true`; escondendo a superfície em `onDragStart`,
  idem; sem alvo, `dropped: false`; depois do arrasto, um clique fora do
  conteúdo volta a dispensar o overlay.
- No Hyprland o comportamento foi lido no código, não testado na tela.
- Sem ícone de arrasto por enquanto (`start_drag` com `icon` nulo): o cursor
  é o do compositor. Um ícone exigiria uma superfície própria; o Hyprland
  aborta o arrasto se ela for destruída antes do fim.
- Só Wayland. Em X11, `start_drag` devolve `false` e nada acontece; o
  terminal ignora `drag`.
- Durante o arrasto, nenhuma parte da superfície de origem é alvo (nem o
  próprio conteúdo). Soltar de volta no launcher não faz nada.
