# 0016 — Dados pertencem à extensão; estado compartilhado e módulos de desktop

## Contexto
Até aqui um sinal criado por módulo pertencia ao código que o criou (ADR
0002): uma assinatura feita dentro de um binding morria quando ele
re-executava. Uma camada de dados preguiçosa (`status.net()` lido em vários
bindings) perdia a assinatura a cada re-execução; assinar no topo de um
módulo compartilhado obrigava toda entrada que o importa a ter a capacidade.

## Decisão
`ModuleCx::root_signal` cria um sinal **da extensão**. Módulos de dados
usam-no, e seus prelúdios cacheiam por chave: `use("audio").subscribe("state")`,
`system.cpu()`, `ctx.now()` e `shared("panel")` devolvem sempre o mesmo sinal,
chamados de onde for. A assinatura dura até a extensão descarregar.

Módulos novos (crates `tessela-state` e `tessela-desktop`), cada valor um
*tópico* produzido numa thread que o primeiro assinante liga:

| módulo | global | capacidade |
|---|---|---|
| `state` | `shared(key, initial, { store })` | `state` |
| `context` | `ctx` | `context` |
| `hyprland` | `hyprland` | `hyprland`, `hyprland:dispatch` (sem `exec`) |
| `system` | `system` | `system` |
| `apps` | `apps` | `apps`, `apps:launch` |
| `tray` | `tray` | `tray` (ids checados contra os itens descobertos) |

Trabalho lento (conectar ao barramento, varrer `.desktop`, indexar ícones)
nunca roda na thread do worker que pediu.

## Consequências
- Acessores de dados podem ser chamados em qualquer lugar; cada superfície só
  precisa das capacidades do que lê.
- Assinaturas por binding (uma por item de uma lista que muda) acumulam até a
  extensão descarregar; até agora nenhum app precisou disso.
- Serviços externos continuam existindo como *plugins* do projeto: o runner
  os inicia, reinicia e mata junto com ele.
