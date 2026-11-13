# Front web do simple-core — design

## Intenção

O `simple-core` é um **template**. Um projeto real nasce dele com `scripts/new-project.sh <nome>`: o script copia,
renomeia tudo e cria um repositório novo e independente, que nunca mais volta a puxar mudanças do template.

Hoje o template entrega só a API. Este design acrescenta o front web **no mesmo template**, na pasta `web/`, para que
todo projeto novo já nasça com API e interface prontas e em sincronia: login, gestão de tenants, usuários, perfis e
permissões funcionando, com bom acabamento. Cada projeto real constrói as próprias telas de negócio em cima disso.

Decisões do dono do projeto:

- O front fica em `web/`, dentro do repositório do `simple-core`, e não em um repositório `simple-web` separado.
  Motivo: projetos reais usam os dois juntos, e assim a API, a especificação OpenAPI, os tipos e as telas mudam no
  mesmo commit.
- O template precisa ser fácil de renomear, de mudar de marca e de estender com telas novas (requisitos de template,
  abaixo).
- O plano original foi revisado: 3D, mapas, a rota `/lab` e Motion saíram do escopo.

## Objetivos

- Front para toda a API atual: plataforma (tenants, admins, superusuários, meu perfil) e tenant (usuários, acesso do
  usuário, perfis, meu perfil).
- Alto desempenho, interface bonita, animações fluidas e acessibilidade WCAG 2.2 AA.
- Segurança de sessão: access token só em memória, refresh token só em cookie `HttpOnly`.
- Ser um bom template: renomeável pelo script, marca trocável num só lugar e um caminho documentado para telas
  novas.

## Não objetivos

SSR, PWA offline, aplicativo mobile, temas por tenant, 3D, mapas, bibliotecas de animação (Motion e similares),
gerenciamento de estado global além do necessário, bibliotecas de componentes com estilo pronto e telas de negócio de
exemplo.

## Antiobjetivos (seria um fracasso mesmo funcionando)

- Um token aparecer em `localStorage`, `sessionStorage` ou em cookie legível pelo JavaScript.
- O front decidir permissão no lugar da API, ou recalcular regras que a API já calcula.
- Um projeto novo sair do `new-project.sh` com "simple-core" sobrando no front, ou com o front fora de sincronia com
  a especificação da API.
- Duas abas abertas derrubarem a sessão do usuário por reuso de refresh token.
- Adicionar uma tela nova exigir entender o template inteiro.

## Stack

- SvelteKit com Svelte 5 (runes), TypeScript `strict` e `adapter-static`, com SSR desligado e fallback para
  `index.html`
- Vite e pnpm
- Tailwind CSS com tokens de design em variáveis CSS
- Bits UI para componentes acessíveis sem estilo imposto
- TanStack Query para Svelte e TanStack Virtual
- `openapi-typescript` e `openapi-fetch`, gerados a partir da especificação da API
- Zod para validação de formulários
- Paraglide para internacionalização, com `pt-BR` como idioma padrão (único idioma entregue)
- Vitest, Testing Library, Playwright, axe-core e Lighthouse CI

Use as versões estáveis mais recentes e faça o commit do lockfile.

## Requisitos de template

### Renomear junto

- O `scripts/new-project.sh` já troca `simple-core`/`simple_core` em todo arquivo de texto versionado; ele passa a
  cobrir o front também (nome do pacote em `web/package.json`, título da página, nome no menu e textos).
- O README gerado pelo script ganha uma seção do front (rodar, testar, estrutura) e a receita de tela nova.
- A verificação final do script, que avisa sobre referências ao nome antigo, também cobre `web/`.
- O nome exibido na interface vem de uma única constante de marca (abaixo), escrita como `simple-core` no template,
  para o script trocá-la. Formas derivadas (ex.: título com maiúsculas) são calculadas a partir dela, nunca escritas à
  mão.

### Marca trocável num só lugar

Um único arquivo, `web/src/lib/brand.ts` (ou equivalente com o CSS de tokens ao lado), concentra: nome do produto,
logo (SVG), cor principal e de destaque (de onde a escala de cores é derivada) e a fonte. Mudar a cara do projeto novo
é editar esse arquivo. Componentes nunca usam valores de cor, raio ou fonte soltos, só tokens.

### Caminho claro para telas novas

O README do front documenta a receita, passo a passo, para um módulo novo de negócio (ex.: "pedidos"):

1. API: módulo e features no catálogo (`src/access/catalog.rs`), rotas com a regra `Access`, `#[utoipa::path]`.
2. `make api-types` para atualizar a especificação e os tipos.
3. Rotas em `web/src/routes/t/[slug]/<modulo>/`, usando os componentes base (lista virtualizada, formulário,
   confirmação, estados de carregamento/vazio/erro).
4. Uma entrada no arquivo de menu (`web/src/lib/nav.ts`): rótulo, ícone, caminho e feature exigida. O item aparece
   sozinho só para quem tem a feature, e a rota ganha a guarda de 403 a partir da mesma entrada.
5. Mensagens no i18n, inclusive os códigos de erro novos que a API passar a devolver.

O template não traz módulo de exemplo; a receita usa os módulos existentes (usuários, perfis) como referência.

## Repositório

- Front em `web/`. O `Dockerfile` da raiz continua sendo o da API; o do front fica em `web/Dockerfile`, e o Nginx em
  `web/docker/nginx.conf`.
- `release-please`: API e front formam um produto só, com **uma versão única** para o repositório. A release passa a
  atualizar também a versão em `web/package.json` (`extra-files`).
- CI: um workflow novo roda os testes da API e os do front (ver Etapas).

## Arquitetura

- O front é servido pela mesma origem da API. Em produção, o Nginx entrega os arquivos estáticos e encaminha `/v1` e
  `/health` para a API. Assim não há CORS e os cookies `SameSite=Strict` funcionam.
- Em desenvolvimento, o Vite encaminha `/v1` e `/health` para a API usando `API_PROXY_TARGET`
  (padrão `http://127.0.0.1:8080`).
- O front usa somente caminhos relativos.
- O cliente HTTP é gerado a partir da especificação. Chamadas `fetch` diretas fora de `web/src/lib/api` são
  proibidas (regra de lint).
- `make api-types` roda `make openapi OPENAPI_OUT=web/openapi.json` e depois o `openapi-typescript`. O
  `web/openapi.json` e os tipos gerados são versionados, e o CI falha se estiverem desatualizados.

## Contrato da API usado pelo front

### Autenticação

| Ação | Plataforma | Tenant |
|---|---|---|
| Login | `POST /v1/platform/auth/login` | `POST /v1/auth/login` + `X-Tenant-Slug` |
| Refresh | `POST /v1/platform/auth/refresh` | `POST /v1/auth/refresh` + `X-Tenant-Slug` |
| Logout | `POST /v1/platform/auth/logout` + Bearer | `POST /v1/auth/logout` + Bearer |
| Sessão | `GET /v1/platform/me` | `GET /v1/me` |
| Cookie | `sc_platform_rt` (`Path=/v1/platform/auth`) | `sc_tenant_rt` (`Path=/v1/auth`) |

- Toda chamada de login, refresh e logout envia `X-Auth-Mode: cookie`. Nesse modo o corpo da resposta não traz
  `refresh_token`, e refresh e logout não levam corpo.
- O access token vale `expires_in` segundos (15 min).
- Um 401 no refresh apaga o cookie: a sessão acabou.
- Reusar um refresh token já rotacionado revoga todas as sessões do usuário. Por isso o refresh precisa ser único por
  aba e coordenado entre abas (ver Autenticação).
- Só existe **uma sessão de tenant por navegador** (um cookie). A API substituiria o cookie num novo login, mas o
  front nunca deixa isso acontecer sem o usuário sair antes (ver Sessões).
- O refresh de tenant exige `X-Tenant-Slug`. O front guarda o slug da sessão de tenant atual em `localStorage`
  (não é dado sensível) para conseguir restaurá-la ao recarregar.

### Recursos

- Paginação: `?limit=` (1 a 100, padrão 50) e `?after=<next_after>`. A resposta é `{ items, next_after }`, e
  `next_after` é `null` na última página.
- Tenants: `GET/POST /v1/platform/tenants`, `GET/PATCH /v1/platform/tenants/{id}`,
  `POST /v1/platform/tenants/{id}/retry`. `status` é `provisioning`, `active`, `failed` ou `suspended`; `last_error`
  vem quando `failed`. Criar devolve `202` com `provisioning`.
- Admins de tenant: `GET/POST /v1/platform/tenants/{id}/admins`, `DELETE /v1/platform/tenants/{id}/admins/{user_id}`.
- Superusuários: `GET/POST /v1/platform/superusers`, `PATCH/DELETE /v1/platform/superusers/{id}`.
- Usuários: `GET/POST /v1/users`, `GET/PATCH/DELETE /v1/users/{id}`.
- Catálogo de features por módulo: `GET /v1/modules`.
- Perfis: `GET/POST /v1/profiles`, `GET/PATCH/DELETE /v1/profiles/{id}`, `PUT /v1/profiles/{id}/features`.
- Acesso do usuário: `GET /v1/users/{id}/access`, `PUT /v1/users/{id}/profiles`,
  `PUT/DELETE /v1/users/{id}/overrides/{feature_code}`.
- Meu perfil de plataforma: `PATCH /v1/platform/me`. Meu perfil de tenant: `PATCH /v1/users/{me.id}` (o próprio
  usuário altera o nome sem feature; a senha exige `current_password`).

### Erros

- Todo erro é `application/problem+json` com `code` estável. As mensagens da interface são traduzidas pelo `code`;
  `detail` e `message` (em inglês) nunca são exibidos ao usuário.
- No 422, `errors[]` traz `field` e `code` para cada campo. O formulário mapeia `field` para o campo correspondente
  e `code` para a mensagem; `field = "body"` vira erro geral do formulário.
- A lista de códigos vem da própria API (Etapa 1): os códigos de 403/409 já estão em `src/error.rs`
  (`error::reason`), e os códigos de campo passam a ser constantes no mesmo lugar; a especificação OpenAPI lista
  todos como valores possíveis (`enum`) de `Problem.code` e `FieldError.code`. O arquivo de mensagens do Paraglide
  tem uma entrada para cada código, e um teste unitário compara com o `web/openapi.json`: se a API ganhar um código
  sem tradução, o CI do front falha. *(Decisão de design, não do dono do projeto.)*

## Rotas

```text
/                               redireciona conforme a sessão
/platform/login
/platform                       painel da plataforma
/platform/tenants
/platform/tenants/[id]          detalhes, status e admins
/platform/superusers            somente owner
/platform/me
/t/[slug]/login
/t/[slug]                       início do tenant
/t/[slug]/users
/t/[slug]/users/new
/t/[slug]/users/[id]            dados do usuário
/t/[slug]/users/[id]/access     perfis, exceções e permissões efetivas
/t/[slug]/profiles
/t/[slug]/profiles/[id]         features agrupadas por módulo
/t/[slug]/me
```

- O slug do tenant fica na URL. O front envia `X-Tenant-Slug` no login e no refresh de tenant e em todas as
  requisições de usuários de plataforma dentro de `/t/[slug]`.
- Um usuário de plataforma entra em qualquer tenant a partir de `/platform/tenants`, usando a própria sessão de
  plataforma. Dentro de `/t/[slug]`, ele não tem `/v1/me` (essa rota exige token de tenant): o front o trata como
  acesso total, e `/t/[slug]/me` redireciona para `/platform/me`.
- A tela de login de tenant lembra o último slug usado.

## Sessões (decisão do dono do projeto)

**Usuário de tenant: fica logado e precisa sair para trocar de tenant.**

- A sessão continua após recarregar a página e fechar o navegador, enquanto o refresh token for válido (30 dias,
  renovado a cada uso).
- Com uma sessão no tenant `acme`, abrir qualquer rota de `/t/beta` (inclusive `/t/beta/login`) mostra uma tela
  "Você está conectado em acme" com dois botões: **Voltar para acme** e **Sair de acme e entrar em beta**. O
  segundo faz logout de `acme` e abre o login de `beta`. O front nunca envia um login de tenant enquanto existir
  outra sessão de tenant.
- O logout vale para todas as abas: as abas em `/t/acme` vão para o login de `acme`.

**Owner e superusuário: acessam todos os tenants sem sair de nada.**

- Usam a sessão de plataforma com `X-Tenant-Slug` em qualquer `/t/[slug]`, inclusive vários tenants ao mesmo tempo
  em abas diferentes (não há cookie de tenant envolvido).
- Se houver sessão de plataforma, ela vale dentro de `/t/[slug]`, mesmo que também exista uma sessão de tenant no
  navegador: quem é owner ou superusuário precisa ver tudo.
- Sem sessão de plataforma, `/t/[slug]` usa a sessão de tenant daquele slug, ou leva ao login do tenant.

## Autenticação

- O access token fica somente em memória, nunca em `localStorage`, `sessionStorage` ou cookie acessível ao JavaScript.
- Ao abrir a aplicação, tente o refresh de plataforma e, se houver slug guardado, o de tenant, para restaurar as
  sessões.
- Renove o access token cerca de 60 s antes de expirar.
- Em resposta 401, faça um único refresh compartilhado entre todas as requisições pendentes e repita cada requisição
  uma vez. Se o refresh falhar, encerre a sessão.
- **Coordenação entre abas:** só uma aba renova por vez, usando a Web Locks API (`navigator.locks`) com um lock por
  escopo. Sem isso, duas abas podem usar o mesmo refresh token e a API revoga a sessão inteira por reuso. A aba que
  renovou avisa as outras pelo `BroadcastChannel`, e cada uma faz o próprio refresh na sequência (o cookie já é o
  novo).
- O `BroadcastChannel` sincroniza login e logout de cada escopo, sem enviar tokens.
- As sessões de plataforma e de tenant são independentes.
- O logout exige access token válido. Se ele estiver expirado, renove antes de chamar o logout. Em qualquer caso,
  limpe o estado local.
- Depois do login, redirecione para a rota originalmente pedida, aceitando somente caminhos internos.

## Permissões na interface

- Após o login, carregue `/v1/me` ou `/v1/platform/me` e guarde o resultado em um store de sessão.
- Crie uma função `can(feature)` e um componente `<Can>`. Para usuários de tenant, `can` consulta `features` do
  `/v1/me` (a API já devolve todas as features para admins). Owner e superusuário têm acesso total dentro dos tenants.
- Esconda itens de menu e botões sem permissão. Rotas acessadas sem permissão mostram uma página 403.
- Nas telas de exceções e perfis, mostre apenas features que o usuário atual pode conceder. A API recusa o resto com
  `cannot_grant`.
- Telas exclusivas do owner só aparecem quando `role` for `owner`.
- Após alterar perfis ou exceções do próprio usuário, recarregue a sessão.
- Um 403 da API é a fonte de verdade, mesmo que a interface tenha permitido a ação.

## Telas

### Plataforma

- **Tenants:** lista com nome, slug e status em badge. O owner cria tenants, edita o nome, suspende, reativa e tenta
  novamente os que falharam, vendo `last_error`.
- **Provisionamento:** após o `202`, mostre o tenant como "provisionando" e consulte o status a cada 2 s, com backoff
  até 10 s, até `active` ou `failed`.
- **Admins de tenant:** o owner cria e remove admins na página do tenant.
- **Superusuários:** o owner cria, edita, desativa e exclui. O owner nunca aparece como editável.
- **Meu perfil:** nome e troca de senha com `current_password`.

### Tenant

- **Usuários:** lista com rolagem infinita usando `next_after`, virtualizada. Criação, edição e exclusão com
  confirmação.
- **Acesso do usuário:** perfis atribuídos, exceções `allow` e `deny` com motivo e expiração opcional, e as
  permissões efetivas com a origem de cada uma (perfil, exceção ou acesso total), vinda de `sources` na API (Etapa 1).
  O front não recalcula a regra.
- **Perfis:** lista, criação, edição, exclusão e matriz de features agrupadas por módulo (`/v1/modules`), com
  seleção por módulo inteiro.
- **Meu perfil:** nome e troca de senha com `current_password`.

### Comportamento comum

- Os formulários validam no cliente com as mesmas regras da API (e-mail até 254 caracteres; nome de 1 a 120; senha
  de 12 a 128; slug `^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$`; motivo de 1 a 500; expiração no futuro) e mapeiam
  `errors[]` do Problem para os campos.
- Mostre mensagens específicas para 401, 403, 404, 409, 429 e 503. No 429 e no 503, informe que a operação pode ser
  repetida e, quando houver `Retry-After` (Etapa 1), em quanto tempo.
- Use atualizações otimistas em edições simples, revertendo com aviso em caso de erro.
- Exclusões pedem confirmação. Ações destrutivas sobre admins e superusuários mostram o nome do alvo na confirmação.
- Toda tela tem estados de carregamento com skeleton, lista vazia e erro com opção de tentar novamente.

## Design visual

- Tokens para cores, tipografia, espaçamentos, raios, sombras, durações e curvas de animação, alimentados pelo arquivo
  de marca.
- Tema claro, escuro e automático, sem piscar a tela ao carregar.
- Layout responsivo de 360 px a telas largas, com menu lateral recolhível no desktop e gaveta no mobile.
- WCAG 2.2 AA: contraste, foco visível, navegação por teclado, rótulos e anúncios para leitores de tela.
- Fonte variável auto-hospedada, com `font-display: swap` e somente os caracteres usados em português.
- A linguagem visual (paleta, densidade, estilo dos componentes) fica a cargo de quem constrói, dentro dessas regras.

## Animações

- Durações de 120 a 250 ms na interface e até 400 ms em transições de página.
- Anime somente `transform`, `opacity` e `filter`. Nunca anime largura, altura, `top` ou `left`.
- View Transitions API na navegação entre páginas, com fallback sem animação.
- Transições nativas do Svelte em listas, modais, toasts e menus.
- Com `prefers-reduced-motion`, substitua movimentos por mudanças de opacidade ou remova a animação.
- Nenhuma animação pode atrasar uma interação ou impedir um clique.

## Desempenho

Orçamentos, verificados no CI:

- JS inicial da tela de login: até 100 KB comprimido
- JS inicial das telas de tenant: até 150 KB comprimido
- CSS total: até 30 KB comprimido
- Lighthouse em perfil mobile com throttling: desempenho, acessibilidade e boas práticas acima de 95
- LCP abaixo de 2,5 s, INP abaixo de 200 ms e CLS abaixo de 0,1

Regras:

- Code splitting por rota e por área: plataforma e tenant em chunks separados.
- Pré-carregue rotas no hover ou foco dos links.
- TanStack Query com `staleTime` por tipo de dado, sem refetch automático ao focar a janela em listas grandes.
- Virtualize qualquer lista que possa passar de 100 itens.
- Sem bibliotecas inteiras de datas, ícones ou utilitários quando uma importação parcial resolver. `Intl` para datas
  e números.
- Imagens em AVIF ou WebP, com dimensões declaradas.
- Relatório de bundle no CI, falhando se algum orçamento for ultrapassado.

## Segurança

- CSP sem `unsafe-inline` e sem `unsafe-eval`, com `connect-src 'self'`.
- Headers `Strict-Transport-Security` (só quando servido por HTTPS), `X-Content-Type-Options`, `Referrer-Policy` e
  `frame-ancestors 'none'`.
- Nunca usar `{@html}` com dados da API.
- Nunca registrar tokens, senhas ou respostas de autenticação no console ou em ferramentas de erro.
- Limpar o cache do TanStack Query no logout e ao trocar de sessão ou de tenant.

## Docker e comandos

- `web/Dockerfile` multi-stage: build com Node e imagem final com Nginx sem privilégios de root.
- O Nginx serve arquivos com hash usando `Cache-Control: public, max-age=31536000, immutable` e `index.html` com
  `no-cache`; encaminha `/v1` e `/health` para `API_UPSTREAM`, com compressão Brotli ou gzip e fallback de rotas para
  `index.html`; envia `X-Forwarded-For` com o IP real do cliente.
- Serviço `web` no `compose.yaml`, no perfil `web`, dependendo da `api` saudável. `make up` e `make up-external`
  passam a subir também o `web`; o front publicado passa a ser a porta de entrada.
- `make dev-web` roda o Vite com proxy para a API local. `make api-types` atualiza especificação e tipos.
- Em desenvolvimento, o cookie `Secure` funciona em `http://localhost`. Para acessar por outro host sem TLS, use
  `AUTH_COOKIE_SECURE=false`.

## Estrutura

```text
web/src/lib/api/          cliente gerado, interceptadores e erros
web/src/lib/auth/         sessão, refresh, locks e sincronização entre abas
web/src/lib/access/       can(), <Can> e helpers de permissão
web/src/lib/ui/           componentes base
web/src/lib/motion/       tokens e helpers de animação
web/src/lib/i18n/
web/src/lib/brand.ts      marca do projeto
web/src/lib/nav.ts        menu e guardas de rota por feature
web/src/routes/platform/
web/src/routes/t/[slug]/
web/tests/unit/
web/tests/e2e/
web/docker/nginx.conf
web/Dockerfile
web/openapi.json
```

## Etapas

1. **Backend:**
   - `ClientIp` passa a aceitar `X-Forwarded-For` somente de proxies confiáveis (lista configurável). Sem isso,
     atrás do Nginx todos os clientes dividiriam o mesmo limite de login.
   - `sources` em `GET /v1/users/{id}/access`: para cada feature efetiva, a origem (perfil com id, `allow` ou acesso
     total de admin), e as features bloqueadas por `deny`.
   - `Retry-After` no 429.
   - Códigos de erro de campo como constantes em `src/error.rs`, e todos os códigos (Problem e campo) listados como
     `enum` na especificação OpenAPI, com teste que garante que a lista cobre todos os usados.
   - CI de testes da API (fmt, clippy, `sqlx-check`, testes com PostgreSQL).
2. Projeto em `web/`, marca, tokens de design, tema e componentes base.
3. Cliente gerado (`make api-types`) e tratamento de erros por `code`.
4. Autenticação de plataforma e de tenant, refresh com lock e sincronização entre abas.
5. Sessão, `can()`, `<Can>` e `nav.ts`.
6. Telas da plataforma.
7. Telas do tenant.
8. Animações e View Transitions.
9. Nginx, Dockerfile, Compose, `new-project.sh`, README gerado e release-please.
10. Testes, orçamentos e Lighthouse no CI.
11. Medir, corrigir um gargalo por vez e comparar antes e depois.
12. Documentar no README a execução, as decisões técnicas, a receita de tela nova e os resultados.

## Testes obrigatórios

**Unitários**

- `can()` para cada nível de usuário e combinação de perfil e exceção.
- Mapeamento de `errors[]` (RFC 9457) para os campos do formulário, por `field` e `code`.
- Todo `code` da API tem mensagem traduzida.
- Refresh único com várias requisições recebendo 401 ao mesmo tempo.
- Duas abas simuladas não usam o mesmo refresh token.

**E2E com Playwright, contra a API real em Docker**

- Login e logout de plataforma e de tenant, incluindo sincronização entre abas.
- Sessão restaurada ao recarregar a página, sem token em `localStorage`, `sessionStorage` ou `document.cookie`.
- Duas abas abertas por mais tempo que o access token continuam logadas.
- Usuário logado em um tenant que abre outro tenant vê a tela "Você está conectado em..." e só entra no outro depois
  de sair; o logout leva as outras abas para o login.
- Owner com dois tenants abertos em abas diferentes, sem precisar sair de nenhum.
- Com sessão de plataforma e de tenant no mesmo navegador, `/t/[slug]` usa a de plataforma.
- Owner cria tenant, acompanha o provisionamento e cria o admin.
- Superusuário não vê nem acessa telas exclusivas do owner.
- Admin de tenant gerencia usuários, perfis e exceções no próprio tenant.
- Usuário comum vê apenas o que suas features permitem e recebe 403 ao acessar uma rota proibida pela URL.
- Usuário com `deny` não vê a ação correspondente.
- Rolagem infinita na lista de usuários com dados suficientes para várias páginas.
- Erros 409, 422 e 429 exibidos corretamente.
- Navegação completa apenas pelo teclado.
- axe sem violações nas telas principais.
- Interface correta com `prefers-reduced-motion`.

**Template**

- `new-project.sh <nome> --check` gera um projeto cujo front instala, compila e passa nos testes unitários, sem
  nenhuma ocorrência de `simple-core` em `web/`.

## Critérios de aceite

- `pnpm check`, `pnpm lint`, testes unitários e E2E passam.
- `make up` sobe API, banco e front, e o owner do seed consegue entrar pelo navegador.
- Nenhum token aparece em armazenamento do navegador acessível ao JavaScript.
- Todos os orçamentos de desempenho e o Lighthouse passam no CI.
- Um projeto gerado pelo `new-project.sh` sobe com o nome novo na interface.
- O README traz os resultados reais medidos e a receita de tela nova.

## A cargo de quem constrói

Linguagem visual, biblioteca de ícones (com importação parcial), organização interna dos componentes, chaves de
`staleTime`, formato do relatório de bundle e escolha do provedor de CI (GitHub Actions é o padrão do repositório).
