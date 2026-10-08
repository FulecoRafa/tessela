# 0012 — Ações declarativas

## Decisão
Uma prop de evento (`on_click`, `on_press`, `on_release`, `on_scroll`) aceita
uma função do guest **ou** uma ação `{"$action": {module, func, args}}`.
O renderer não distingue: devolve o valor da prop com o *payload* do evento.
O host roda funções como handlers; ações vão direto ao módulo, sem executar
código da extensão. O payload chega aos args como `event`.

Módulos oferecem açúcar: `power:action("cycle_profile")`.

## Consequências
- Capacidades continuam valendo: uma ação forjada para um serviço não
  declarado é recusada pelo módulo.
- Mensagens interativas (ADR 0009) e o controle de jogo (fase 3) não
  precisam executar código no cliente.
- Ações são "dispare e esqueça": recursos que criarem pertencem à raiz.
