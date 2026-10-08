# Arquitetura

> Nome provisório: **tessela** ([ADR 0008](adr/0008-nome-provisorio.md)).

Um framework para apps cuja interface e cujas extensões são escritas em
**Luau ou TSX**, rodam **em sandbox**, falam com o mundo só por **módulos**
declarados em manifesto e são desenhadas por **renderers trocáveis** (GPUI,
terminal, headless; depois remoto, HTML, LVGL...). Também serve para
extensões que não desenham nada.

## Camadas

```
extensões   Luau · TSX                         (código de usuário, sandboxed)
──────────────────────────────────────────────────────────────────────────
engines     tessela-luau · tessela-quickjs     trait EngineFactory / Engine
core        tessela-core                       grafo reativo, ownership, capacidades, quotas,
            tessela-view                       Instance; vocabulário, schemas, patches. SEM I/O.
host        tessela-host                       manifestos, pool de workers, hot reload,
                                               trait Renderer, renderer Headless
──────────────────────────────────────────────────────────────────────────
módulos     tessela-unix · tessela-timer       trait Module — o único caminho de I/O
renderers   tessela-tui · tessela-gpui         trait Renderer
fachada     tessela                            re-exporta tudo atrás de features
apps        apps/marathon                      a shell, primeiro app do framework
```

Dependências só descem: engines, módulos e renderers dependem do core (e o
renderer do host), nunca uns dos outros. Trocar o GPUI por outro renderer, ou
rodar sem renderer nenhum, é não registrar aquela peça.

## O fluxo de uma extensão

```
widget.toml + entry ──▶ Instance (engine + grafo) ──▶ Vocabulary ──▶ Reconciler ──Frame(patches)──▶ Renderer
                            ▲          │ invoke       (valida, fallback)   (ids estáveis)            │
                            │ Inbound  ▼                                                             │ UiEvent
                            └──────  Module  ◀──────── ações declarativas ◀── função ou ação ────────┘
```

1. O host lê `widget.toml`, converte capacidades em `Grants` e limites em `Limits`.
2. A `Instance` escolhe o engine pela extensão do arquivo e roda os prelúdios:
   o do core (signals, `log`, componentes) e o de cada módulo registrado.
   Depois sela o sandbox.
3. O guest devolve uma árvore. Funções viram marcadores: `$bind` (prop
   reativa) ou `$fn` (handler); ações viram `$action`.
4. `render()` resolve os bindings sujos e valida a árvore contra o
   vocabulário (tipos de prop, obrigatórias, filhos).
5. O worker reescreve componentes que o renderer não desenha (fallback) e o
   `Reconciler` transforma a árvore em patches com ids estáveis.
6. Quando o guest chama um módulo (`use("power"):subscribe(...)`), o módulo
   checa a capacidade, cria recursos amarrados ao dono no grafo e responde
   depois por um `Sink` (`Inbound::Signal` ou `Inbound::Callback`).
7. Um evento do usuário volta como o valor da prop: função roda como handler;
   ação vai direto ao módulo, sem código da extensão.
8. Quando um binding re-executa ou a instância morre, o grafo descarta o que
   ela criou e chama `Module::release` para cada recurso.

## Contratos

| Trait | Onde | Responsabilidade |
|---|---|---|
| `EngineFactory` / `Engine` | core | carregar código, chamar funções, converter valores |
| `Module` | core | expor capacidades ao guest; único lugar com I/O |
| `Renderer` | host | aplicar patches, desenhar, devolver eventos; declarar o que desenha (`supports`) |
| extension point | — | só `kind = "view"` por enquanto ([ADR 0006](adr/0006-pontos-de-extensao-sob-demanda.md)) |

Escrever um módulo: implemente `Module` (nome, `invoke`, `release` e um
prelúdio por linguagem que usa os helpers privados `__rt.invoke`, `__rt.wrap`
e `__rt.define`). `tessela-unix` é o exemplo de referência.

## Features da fachada

| feature | traz | padrão |
|---|---|---|
| `luau` | engine Luau | ✓ |
| `quickjs` | engine JS/TSX | ✓ |
| `unix` | módulo de serviços por Unix socket | ✓ |
| `timer` | módulo de timers (`after`, `every`) | ✓ |
| `tui` | renderer de terminal | ✓ |
| `gpui` | renderer GPUI (layer-shell) | |

O renderer `Headless` está sempre disponível (testes e benchmarks).

## Garantias de sandbox

Testadas em `crates/tessela/tests/sandbox.rs`:

- sem autoridade ambiente nos dois engines (sem `io`, `require`, `fetch`, `import`...);
- capacidade não declarada é recusada pelo módulo; nomes de serviço não escapam do diretório;
- prelúdios imutáveis; helpers privados removidos antes do código do usuário;
- CPU por entrada, memória por instância, tamanho e profundidade da árvore;
- chamadas a módulos por segundo e recursos vivos por extensão;
- árvores validadas por schema; ações forjadas continuam sujeitas a capacidades;
- descartar a instância libera as assinaturas no serviço.

Não coberto: bugs de memória dentro do Luau ou do QuickJS (são C/C++). O
caminho para isso é rodar o engine em WASM, sem mudar a API do guest.

## Roteiro

| Fase | Entrega | App que valida |
|---|---|---|
| **0** ✓ | camadas, contratos, features, docs | marathon rodando sobre o framework |
| **1** ✓ | ids estáveis e patches, schemas e componentes de app com fallback, eventos de ponteiro, ações declarativas, timers, pool de workers, quotas, `persist` | marathon usando tudo isso, benchmark igual |
| 2 | extensões não-UI (providers, comandos), schemas → tipos, CLI, harness de testes | launcher |
| 3 | modo remoto (patches + eventos pela rede), renderer HTML para celular | launchpad, controle de jogo |
| 4 | pacotes assinados, rede P2P, prompts de capacidade; mensageria interna | "Malha", mensageria |

Decisões registradas em [`docs/adr/`](adr/).
