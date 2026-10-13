# 0018 — Validar pelo devtools, não só por testes

## Contexto
Uma shell não se valida bem só com testes: o que importa é o que ela faz
ligada à sessão de verdade (barramento, Hyprland, apps), quanto custa e como
fica. Precisávamos ver dentro dela enquanto roda, como no DevTools de um
navegador, inclusive com a tela bloqueada.

## Decisão
- O core aceita um `InvokeTrace` por instância (cada chamada a módulo) e o host
  um `Probe` (cargas, renders com seus patches, entregas, eventos, logs). Sem
  eles instalados, nada é medido.
- `tessela run --devtools` serve um socket; `tessela devtools` é o cliente:
  árvore viva com ids (espelhada dos mesmos patches do renderer), console,
  chamadas, eventos, entregas com o nome do tópico, frames com o que mudou,
  desempenho, estado, janelas; e ações: clicar num nó (qualquer handler, com
  payload), editar estado compartilhado, capturar uma superfície.
- `tessela run --nested` renderiza num sway headless próprio: previews e
  capturas sem tela física e sem tocar a barra do desktop, com os módulos
  lendo a sessão real.

## Consequências
- Os defeitos desta fase foram achados assim: assinatura de bandeja travando
  um worker por 611 ms, tiles recriados por título de janela, superfície
  oculta re-renderizando, plugins órfãos, a shell encerrando ao recarregar
  com erro, protocolo de viewport rejeitado pelo sway.
- O cliente lê e escreve só por um socket `0600` do usuário.
