# 0022 — SDK Rust e serviços num processo

## Contexto
A adwaita-shell rodava nove plugins em Python, um interpretador cada. Medido
ocioso, o renderer (`tessela`) gastava ~0,4% de um núcleo e 87 MiB; os
plugins, ~7,5% e ~135 MiB de PSS (13–22 MiB cada). O custo vinha de duas
coisas: um interpretador por serviço e polling de CLIs (`wpctl get-volume`
duas vezes por segundo, `pw-dump` a cada 4 s, `powerprofilesctl` — ele
mesmo um script Python — a cada 5 s, `nmcli` e `bluetoothctl` a cada 10 s).
Quase tudo isso tem eventos no sistema (sinais D-Bus, `pw-dump --monitor`,
inotify). Os SDKs Python e Go servem um serviço por processo.

## Decisão
- **`sdk/rust`, crate `tessela-service`**, irmão dos SDKs Python e Go, com o
  mesmo comportamento no fio (os textos de erro inclusive): `Service` com
  tópicos tipados por serde (`publish` ignora valor igual), métodos
  síncronos e assíncronos (argumentos por serde, `()` responde `null`,
  `Err` responde `error`), `describe`. Chamadas rodam em paralelo, como no
  Python vendorizado da shell.
- **Vários serviços num processo**: `Host` serve qualquer número de
  `Service`, um socket cada, numa thread. É isso que deixa um projeto
  entregar todos os plugins como um binário.
- **Runtime: `async-io` + `async-executor`** (a família smol), monothread
  (`LocalExecutor`): já está no lockfile (é onde o zbus do
  `tessela-desktop` roda), não traz tokio, e handlers podem guardar estado
  em `Rc`/`RefCell`. As conexões zbus do SDK não têm thread própria: o
  executor do host as move. Tokio foi descartado porque o zbus com a
  feature `tokio` exige um runtime tokio e, pela unificação de features do
  workspace, mudaria também o zbus do `tessela-desktop`.
- **Socket**: `$TESSELA_SERVICES_DIR`, senão `$MARATHON_SERVICES_DIR`, senão
  `$XDG_RUNTIME_DIR/marathon/services`; `0600`. Um socket órfão (ninguém
  atende) é substituído; um vivo faz `run` falhar em vez de roubá-lo.
  SIGTERM, SIGINT ou a morte do pai (`PR_SET_PDEATHSIG`) encerram: tarefas
  caem, filhos morrem, sockets são removidos.
- **Ajudantes atrás de features**: `process` (comando com timeout; `follow`,
  que reinicia um comando longo e entrega suas linhas; filhos morrem com o
  serviço) e `dbus` (uma conexão por barramento para o processo todo,
  streams por match rule, `signals_from` que segue o dono de um nome).
- **Runner**: `services = ["audio", ...]` num `[plugins.x]` declara os
  sockets que aquele processo serve. O runner espera por eles (até 3 s)
  antes do primeiro render, em vez dos 300 ms fixos, e o devtools diz quais
  estão de pé. Um caminho relativo com `/` no comando é relativo ao
  projeto (`services/target/release/adwaita-services`). Reinício continua
  por processo: caiu, todos os sockets dele somem e voltam juntos, e o
  connector reassina cada um.
- **Eventos antes de polling**: sinal D-Bus, `--monitor`, inotify. Polling só
  onde o sistema não avisa de forma confiável (brilho no sysfs a cada 2 s,
  cotas de IA a cada 5 min, clima a cada 30 min).

## Consequências
- A adwaita-shell troca nove interpretadores por um binário. Medido em
  `--headless`, 120 s ociosos, mesma máquina: plugins Python 12,6% de um
  núcleo, ~141 MiB de PSS, 13 processos; serviços Rust 0,33% (0,22% do
  binário, 0,11% do `pw-dump --monitor`), 9 MiB, 2 processos. A árvore
  inteira (runner + serviços) foi de 13,0% e 165 MiB para 0,7% e 32 MiB.
- Um projeto com serviços em Rust deixa de ser só arquivos: é preciso
  compilar o binário (os pacotes compilam; some a dependência de `python3`).
- Um pânico derruba todos os serviços do processo de uma vez; o runner
  reinicia o processo. É o preço de compartilhar memória e conexões.
- Sair com o pai é o padrão: rodar o binário com `nohup` de um shell que
  termina encerra o serviço (`keep_running_without_parent` desliga).
- Os SDKs Python e Go continuam para serviços pequenos e fora do tessela; o
  protocolo é o mesmo (`tessela-proto`).
