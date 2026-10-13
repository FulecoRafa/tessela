# 0021 — Movimento no renderer: o `Slider` que anda e ondula

## Contexto
O cartão de mídia da shell de referência desenha o progresso como no
Android 13: o que já tocou é uma senoide que corre para a direita, o resto é
uma linha reta apagada, e uma pílula marca a posição. A onda anda a cada
quadro enquanto toca e achata, suavizada, ao pausar; a posição avança em
tempo real entre as leituras do MPRIS (que não avisa a posição); clicar ou
arrastar na linha busca. No QML isso é um `Canvas` repintado por um
`FrameAnimation`.

Aqui, o guest roda num sandbox com orçamento de CPU, conversa com o
renderer por patches e não tem `Canvas` livre
([ADR 0003](0003-vocabulario-minimo-de-ui.md)). Animar pelo guest seria um
timer de 16 ms reavaliando bindings, validando a árvore e mandando patches,
60 vezes por segundo, por algo que não depende de nenhum dado novo. É também
o primeiro caso de animação da shell; o próximo assunto é animação em geral
(transições de largura e cor, painéis deslizando).

## Decisão
- **O que depende só do tempo mora no renderer.** O guest declara o estado e
  a regra; o renderer calcula cada quadro. Nenhum código do guest roda entre
  dois dados novos.
- No `Slider` ([ADR 0020](0020-paineis-e-controles-continuos.md)):
  - `rate` (unidades de `value` por segundo) e `at` (ms desde a época, quando
    `value` era verdade; sem `at`, quando o renderer o recebeu): o nível
    mostrado é `value + rate × (agora − at)`, limitado a 0..1. O plugin
    reporta a posição de tempos em tempos com o instante da leitura; o
    renderer interpola entre os relatórios. `at` absoluto deixa a posição
    certa também quando o nó é recriado (a gaveta reabre, o cartão muda de
    tamanho) sem um relatório novo.
  - `wave` (amplitude em px), `wavelength` e `speed`: com `wave`, o slider é
    desenhado como linha (onda, respiro, trilho reto, pílula). A amplitude
    segue `wave` suavizada (`8/s`, como a referência) e a fase anda enquanto
    o alvo não é zero: pausar é `wave={0}`, e a onda achata no lugar.
  - `onCommit({ value })`: uma vez, ao soltar o ponteiro depois de clicar ou
    arrastar, com resolução de milésimos. O slider continua mostrando o valor
    soltado até o guest mandar um `value` novo (no máximo 2 s), para a linha
    não voltar ao valor antigo enquanto a busca vai e volta pelo serviço. A
    roda só age com `onChange` (volume); com `onCommit` ela rola o que está
    em volta.
- Quadros, no GPUI: o estado por nó (`Motion`: amplitude, fase, âncora,
  valor segurado) fica no modelo do renderer. Ao montar o slider, o renderer
  avança esse estado e, se algo ainda se move, pede o próximo quadro
  (`request_animation_frame`) só para a janela onde ele está. Janela
  fechada (superfície oculta, painel fechado) não desenha e, portanto, não
  pede quadros; pausado e assentado, também não. Se só o nível anda (sem
  onda), o renderer agenda um redesenho por timer, a cada ~meio pixel (entre
  30 e 1 por segundo), em vez de quadros contínuos.
- Fallback: `Progress` com o nível já avançado até o momento do lowering. O
  terminal mostra a posição de cada relatório, sem animação.
- `Image` ganha `radius` (recorta) e `fit: "cover"` (capas que não são
  quadradas).

## Consequências
- Uma animação contínua custa ao guest zero: o plugin manda um relatório por
  segundo, como antes, e o resto é do renderer.
- No GPUI um quadro de animação redesenha a janela inteira daquele slider (o
  GPUI reconstrói a árvore de elementos da janela notificada). Medido no
  `--nested` (build release, Vulkan em software): a gaveta aberta tocando
  monta cada quadro em ~0,2 ms e usa ~9% de um núcleo; pausada ou fechada,
  nenhum quadro. É o custo a vigiar quando houver muitas animações: o passo
  seguinte é isolar partes animadas em views com cache.
- O padrão para as próximas animações fica dado: props declaram alvo e
  regra (`at`, `rate`, alvo suavizado), o renderer interpola e só pede
  quadros enquanto algo visível se move. Transições de largura e cor devem
  seguir o mesmo caminho, sem `Canvas` nem timers no guest.
- `at` usa o relógio de parede da máquina; num renderer remoto ([ADR
  0005](0005-renderer-como-contrato.md)) valem os relógios dos dois lados, o
  que basta para posição de mídia, mas não para sincronismo fino.
- A amplitude da referência segue o volume do áudio (cava). Aqui ela é fixa;
  seguir o áudio seria `wave` vindo de um serviço a poucos Hz, suavizado pelo
  renderer, sem mudar a primitiva.
