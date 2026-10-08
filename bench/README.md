# marathon (GPUI) vs Quickshell

Mesma barra (relógio, CPU/memória, bateria, perfil), mesmos serviços Go/Python,
mesmos sockets, mesmo compositor. `shell.qml` é a versão idiomática em Quickshell;
`bench.py` sobe sway headless, os serviços e cada barra, e mede.

```sh
QS_BIN=/caminho/quickshell python3 bench/bench.py --runs 3   # precisa de sway e grim
```

![as duas barras sob o contador de 100 Hz: marathon em cima, Quickshell embaixo](../docs/bench-compare.png)

## Resultado (2026-10-08)

Quickshell v0.3.1 (Qt 6.9.2, RelWithDebInfo, jemalloc) vs marathon release.
3 rodadas alternando a ordem; mediana (mín–máx). Sem GPU: wlroots pixman +
Mesa llvmpipe/lavapipe, então tudo é rasterizado na CPU nos dois.

|                     | marathon (GPUI) | Quickshell |
|---------------------|-----------------|------------|
| primeiro frame      | 0,18 s (0,16–0,23) | 0,19 s (0,19–0,23) |
| PSS ocioso          | 139 MiB | 171 MiB |
| PSS sem Mesa¹       | ~67 MiB | ~104 MiB |
| CPU ocioso (1 Hz)   | 0,5 % | 0,2 % |
| CPU com contador 100 Hz | 14,4 % | 10,9 % |
| binário (stripped)  | 33 MiB, estático² | 4,4 MiB + 43 MiB de libQt6 |

¹ PSS total menos o mapeado em libLLVM e nos drivers do Mesa (~70 MiB em ambos,
artefato do rasterizador por software). O heap ainda inclui alocações do Mesa.
² GPUI, Luau, QuickJS e o transpilador oxc, tudo dentro do binário.

Onde vai o CPU a 100 Hz no marathon (por thread): thread da UI 5,8 %, llvmpipe
5,0 %, threads do Mesa 1,9 %, runtime Luau do widget 0,5 %, conector 0,4 %.
Os renders são limitados a ~61/s pelo compositor nos dois.

## Ressalvas

- Rasterização por software infla CPU e memória dos dois; compare razões, não
  absolutos. Com GPU real os dois caem bastante, e não necessariamente na mesma proporção.
- A barra QML guarda o estado de cada serviço num objeto só (`values`), o que
  reavalia mais bindings que o necessário; o marathon reavalia só o binding
  afetado. Para uma barra deste tamanho, isso não pesa.
- Duas correções no marathon vieram desta medição: a fonte `"monospace"` não
  existe para o GPUI (era um alias do fontconfig) e cada medição de texto gerava
  um erro — com `RUST_BACKTRACE=1`, com backtrace (40 % de CPU a 100 Hz); e a
  entrega de frames passou de polling a cada 16 ms para um canal que acorda a UI.

## A/B entre fases (mesma máquina, intercalado, 3 rodadas)

`MARATHON_BIN`, `MARATHON_CONFIG` e `MARATHON_STRESS_CONFIG` permitem comparar
dois builds com seus próprios widgets. Mediana (mín–máx):

|                     | fase 0 | fase 1 |
|---------------------|--------|--------|
| primeiro frame      | 0,32 s (0,28–0,42) | 0,28 s (0,27–0,32) |
| PSS ocioso          | 138 MiB | 138 MiB |
| CPU ocioso (1 Hz)   | 0,88 % (0,88–0,92) | **0,48 %** (0,44–0,48) |
| CPU com contador 100 Hz | 29,3 % (28,7–29,4) | 29,7 % (28,5–30,6) |

O ocioso caiu porque patches vazios não acordam o renderer: quando um
serviço publica um valor que não muda o que está na tela, nada é redesenhado.
Esta máquina é mais lenta que a da tabela acima (os dois builds e o
Quickshell dobraram de CPU nela), por isso só vale comparar dentro de cada tabela.
