# 0008 — Nome provisório e licença

## Decisão
Os crates usam o prefixo provisório `tessela` até haver um nome definitivo.
Renomear é mecânico:

    git grep -l tessela | xargs sed -i 's/tessela/NOVO/g'
    for d in crates/tessela*; do git mv "$d" "${d/tessela/NOVO}"; done

O projeto é open source sob **MIT OR Apache-2.0**, o padrão do ecossistema
Rust (os dois textos estão na raiz).

A shell continua se chamando **marathon**; a rede de apps simples, se sair
do papel, é a **Malha**.
