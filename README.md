# tessela

> Nome provisório ([ADR 0008](docs/adr/0008-nome-provisorio.md)). Licença MIT OR Apache-2.0.

Framework para apps cuja interface e cujas extensões são escritas em **Luau ou
TSX**, rodam **em sandbox de verdade**, alcançam o mundo só por **módulos**
declarados no manifesto e são desenhadas por **renderers trocáveis**: GPUI,
terminal ou headless hoje; remoto, HTML e outros no roteiro.

O primeiro app construído com ele é a **marathon**, uma shell para desktop
Linux (`apps/marathon`).

![barra GPUI em layer-shell, dados ao vivo dos serviços Go e Python](docs/gpui-bar.png)

- Arquitetura e roteiro: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- Decisões: [`docs/adr/`](docs/adr/)
- Benchmark contra o Quickshell: [`bench/`](bench/)

## Estrutura

```
crates/
  tessela          fachada: re-exporta tudo atrás de features
  tessela-core     grafo reativo, Instance, traits Engine e Module (sem I/O)
  tessela-view     vocabulário (núcleo + Box, Label, Image, Ring, Slider, Popup, Input, Surface), schemas, patches
  tessela-host     manifestos, pool de workers, hot reload, trait Renderer, renderer Headless
  tessela-luau     engine Luau            (feature luau)
  tessela-quickjs  engine JS/TSX          (feature quickjs)
  tessela-unix     módulo de serviços     (feature unix)
  tessela-timer    módulo de timers       (feature timer)
  tessela-state    estado compartilhado   (feature state)
  tessela-desktop  ctx, hyprland, system, apps, tray (feature desktop)
  tessela-tui      renderer de terminal   (feature tui)
  tessela-gpui     renderer GPUI          (feature gpui): LayerBar e Shell (uma janela por superfície)
  tessela-proto    protocolo dos serviços
apps/tessela       `tessela run` (projetos) e `tessela devtools` (terminal ou `ui`, gráfico)
apps/marathon      a barra de exemplo
vendor/            dependências com correções locais (ver vendor/README.md)
sdk/{go,python}    helpers para escrever serviços
services/          serviços de exemplo (power em Go, sys em Python)
widgets/           widgets da marathon (Luau e TSX)
```

## Montando um app

```rust
use tessela::host::{App, Headless};
use tessela::timer::Timers;
use tessela::unix::UnixServices;

App::builder()
    .engines(tessela::engines())                       // luau + quickjs, conforme as features
    .module(UnixServices::new(["/run/user/1000/marathon/services"]))
    .module(Timers::new())
    .component(meter())?                               // componente do app com fallback (ver apps/marathon)
    .extension("widgets/clock")
    .extension("widgets/battery")
    .build()?
    .run(Headless::new())                              // ou Tui, ou gpui::LayerBar
```

## Rodando um projeto

Um projeto é uma pasta com `tessela.toml`: superfícies (uma entrada TSX ou
Luau por janela), pacotes importáveis como `@nome/...`, plugins (serviços que
o runner mantém vivos) e tokens de tema. Exemplo completo:
[`../adwaita-shell`](../adwaita-shell) (barra, painéis, gaveta, launcher e o pacote `@kit`).

```sh
nix develop                                     # Rust, Go, Python, libs do GPUI, sway, grim
cargo build -p tessela-cli
./target/debug/tessela run ../adwaita-shell --dev        # ao lado da barra do desktop
./target/debug/tessela run ../adwaita-shell --nested     # num sway headless próprio
./target/debug/tessela run ../adwaita-shell --devtools   # e em outro terminal:
./target/debug/tessela devtools                         # overview, tree, calls, frames, click, set, shot...
./target/debug/tessela devtools ui                      # o mesmo numa janela, como o DevTools do navegador
```

Decisões: [ADR 0015](docs/adr/0015-projetos.md) a [0020](docs/adr/0020-paineis-e-controles-continuos.md).

## Rodando a marathon

```sh
./demo.sh                      # barra no terminal (←/→ foco, enter clica, ↑/↓ scroll, mouse funciona)
./demo.sh --headless --seconds 8 --click profile@3 --scroll profile@5:-1
./demo.sh --gpui               # barra GPUI em layer-shell (Wayland)
./check.sh                     # testes + e2e sem display
```

Precisa de Rust, Go e Python 3. Edite qualquer arquivo em `widgets/` com a barra
aberta: o widget recarrega sozinho. Para o GPUI no CachyOS:
`sudo pacman -S wayland libxkbcommon vulkan-icd-loader` e um compositor com
`zwlr_layer_shell_v1` (niri, sway, river, KWin, Hyprland, Wayfire).

## Escrevendo uma extensão

Cada extensão é uma pasta com `widget.toml` e um arquivo de entrada:

```toml
name = "battery"
entry = "battery.luau"              # .luau/.lua ou .tsx/.ts/.jsx/.js
capabilities = ["service:power", "timer"]   # tudo que não está aqui é negado
memory_mb = 16                      # padrão 16
budget_ms = 50                      # CPU por entrada no guest; padrão 50
calls_per_sec = 200                 # chamadas a módulos; padrão 200
max_resources = 256                 # assinaturas, timers... vivos; padrão 256
```

Os dois estilos de reatividade funcionam nas duas linguagens:

```lua
-- Granular (estilo Solid/QML): só a prop que leu o signal re-executa.
local load = use("sys"):subscribe("load")
return Progress { value = function() local v = load(); return v and v.ratio or 0 end }

-- Inteiro (estilo React): retornar uma função faz o widget todo virar um binding.
return function() local b = bat(); return Pill { label = b and b.percent .. "%" or "—" } end
```

```tsx
export default function Clock() {
  const clock = use("sys").subscribe<{ hm: string }>("clock");
  const seconds = signal(false);
  return <Pill icon="clock" label={() => clock()?.hm ?? "--:--"}
               active={seconds} onClick={() => seconds.update((s) => !s)} />;
}
```

Componentes TSX rodam **uma vez**; o que é reativo é função (`label={() => ...}`)
ou o próprio signal (`active={seconds}`). Eventos são `on_*` em Luau e `on*` em
TSX: `click`, `press`, `release` e `scroll` (`{ delta }`).

Mais ferramentas, nas duas linguagens:

```tsx
const seconds = persist("seconds", false);          // sobrevive a hot reload
const stop = every(1000, () => tick.update((n) => n + 1));   // capacidade "timer"; para com o dono
<Pill label="perfil" onClick={power.action("cycle_profile")} />  // ação: o host chama o serviço,
                                                                 // nenhum código da extensão roda
{items().map((i) => <Text key={i.id} text={i.name} />)}         // `key` preserva identidade em listas
```
Tipos para o editor: `widgets/marathon.d.ts` e `widgets/marathon.d.luau`.

## Escrevendo um serviço

Um serviço é um Unix socket `<dir>/<nome>.sock` falando JSON por linha:

```
→ {"id":1,"op":"subscribe","path":"battery"}     ← {"id":1,"event":{...}}   (valor atual, depois cada mudança)
→ {"id":2,"op":"call","method":"x","args":{}}    ← {"id":2,"result":...} | {"id":2,"error":"..."}
→ {"id":3,"op":"get","path":"battery"}           ← {"id":3,"result":{...}}
→ {"id":4,"op":"unsubscribe","sub":1}            → {"id":5,"op":"describe"}
```

`sdk/go/marathon` e `sdk/python/marathon_service.py` implementam isso em ~200
linhas cada, sem dependências. Para testar na mão:
`socat - UNIX-CONNECT:run/services/power.sock`.

## Escrevendo um módulo

Módulos são o único caminho de I/O de uma extensão. Implemente
`tessela::core::Module`: um nome, `invoke` (síncrono, checa capacidades com
`cx.require`, cria signals e recursos amarrados ao dono), `release` e um
prelúdio por linguagem. `crates/tessela-unix` é o exemplo de referência.

## O sandbox

| Camada | Como | Testado em `crates/tessela/tests/sandbox.rs` |
|---|---|---|
| Sem autoridade ambiente | Luau em `sandbox(true)`, sem `require`/`loadstring`/`getfenv`; QuickJS sem `std`/`os`, sem loader de módulos, sem `fetch`/timers; helpers internos removidos antes do código do usuário | `*_has_no_ambient_authority`, `js_cannot_import_anything` |
| Só módulos registrados | sem o módulo `service`, `use` nem existe | `without_the_module_there_is_no_use` |
| Capacidades | o módulo recusa o que o manifesto não declara; nomes de serviço não escapam do diretório | `undeclared_service_is_refused`, `service_names_cannot_escape_the_directory` |
| Prelúdios imutáveis | globais Luau read-only; no JS, `writable:false, configurable:false` | `prelude_is_read_only` |
| CPU e memória | interrupção por deadline (`budget_ms`), limite do alocador (`memory_mb`) | `infinite_loops_*`, `memory_bombs_are_contained` |
| Saída | árvore limitada, validada por schema, só componentes do vocabulário | `huge_trees_are_rejected`, `unknown_components_are_rejected` |
| Quotas | chamadas a módulos por segundo, recursos vivos | `call_rate_is_limited`, `live_resources_are_limited` (em `phase1.rs`) |
| Ações | ação forjada para serviço não declarado é recusada | `forged_actions_still_need_the_capability` |
| Limpeza | descartar a extensão libera as assinaturas no serviço | `dropping_an_instance_releases_subscriptions` |

**Não coberto:** bugs de memória dentro do Luau ou do QuickJS (são C/C++). O
caminho para isso é rodar o engine em WASM, sem mudar a API do guest.
`widgets/intruder` tenta escapar de cada camada; adicione `"intruder"` em `bar`.
