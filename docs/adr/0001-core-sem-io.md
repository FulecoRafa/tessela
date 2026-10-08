# 0001 — O core não faz I/O

## Contexto
No spike, o runtime emitia `Effect`s (subscribe, call, log) e a shell os
executava. Isso provou que um guest pode rodar sem saber onde está — mas os
efeitos eram específicos de serviços.

## Decisão
`tessela-core` não abre arquivos, sockets nem threads. Todo acesso ao mundo é
um `Module`: chamado de forma síncrona pelo guest, respondendo de forma
assíncrona por um `Sink`. Recursos criados por módulos pertencem ao grafo e
são liberados por `Module::release` quando o dono morre.

## Consequências
- O mesmo `Instance` roda no desktop, atrás de um renderer remoto ou num
  visualizador de mensagens com zero módulos.
- Testar o core não precisa de rede nem de mocks de sistema.
- Módulos precisam ser `Send + Sync` e não podem bloquear em `invoke`.
