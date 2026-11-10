// Cenários de carga do simple-core. Selecione com SCENARIO=<nome>.
//
// Cada cenário tem 30 s de aquecimento (tag phase=warmup) e 3 min de medição
// (phase=measure); o resumo considera só a medição. Taxa de chegada constante
// (RATE req/s): se a API não acompanhar, sobram dropped_iterations e o RPS
// medido fica abaixo da taxa pedida.
//
// Tokens de tenant são assinados aqui (HS256) para >= 1.000 usuários reais,
// sem passar pelo Argon2id; o login é medido em cenário próprio.
import http from 'k6/http';
import exec from 'k6/execution';
import crypto from 'k6/crypto';
import encoding from 'k6/encoding';
import { SharedArray } from 'k6/data';

const BASE = __ENV.BASE_URL || 'http://api:8080';
const SCENARIO = __ENV.SCENARIO || 'get_by_id';
const RATE = parseInt(__ENV.RATE || '1000', 10);
const VUS = parseInt(__ENV.VUS || '300', 10);
const WARMUP = __ENV.WARMUP || '30s';
const DURATION = __ENV.DURATION || '180s';
const RUN_ID = __ENV.RUN_ID || 'local';
const ISSUER = __ENV.JWT_ISSUER || 'simple-core';
const OUT = __ENV.OUT || `/load/results/${RUN_ID}`;
const LOAD_PASSWORD = 'load-password-123';

const tenants = new SharedArray('tenants', () => JSON.parse(open('/load/data/tenants.json')));
const meta = JSON.parse(open('/load/data/meta.json'));

// Cada acesso a um elemento de SharedArray desserializa o elemento inteiro
// (o tenant grande tem 20 mil ids): guarda uma cópia por VU.
const tenantCache = [];
const T = (i) => tenantCache[i] || (tenantCache[i] = tenants[i]);

// Vítimas de DELETE: [{t: índice do tenant, id}], embaralhadas de forma
// determinística para espalhar os deletes pelos tenants.
const victims = new SharedArray('victims', () => {
  if (!__ENV.VICTIMS) return [];
  const v = JSON.parse(open(__ENV.VICTIMS));
  let seed = 42;
  const rnd = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
  for (let i = v.length - 1; i > 0; i--) {
    const j = Math.floor(rnd() * (i + 1));
    [v[i], v[j]] = [v[j], v[i]];
  }
  return v;
});

// Distribuição desigual (Zipf, s=1): o tenant i recebe peso 1/(i+1).
const cumulative = (() => {
  const c = [];
  let acc = 0;
  for (let i = 0; i < tenants.length; i++) {
    acc += 1 / (i + 1);
    c.push(acc);
  }
  return c.map((x) => x / acc);
})();

function zipfTenant() {
  const r = Math.random();
  let lo = 0;
  let hi = cumulative.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (cumulative[mid] < r) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

const pick = (arr) => arr[Math.floor(Math.random() * arr.length)];

// ------------------------------------------------------------------ JWT

const b64 = (obj) => encoding.b64encode(JSON.stringify(obj), 'rawurl');
const HEADER = b64({ alg: 'HS256', typ: 'JWT' });

function sign(secret, claims) {
  const body = `${HEADER}.${b64(claims)}`;
  return `${body}.${crypto.hmac('sha256', secret, body, 'base64rawurl')}`;
}

const tokenCache = {};
function tenantToken(ti, sub) {
  const key = `${ti}:${sub}`;
  let t = tokenCache[key];
  if (!t) {
    const iat = Math.floor(Date.now() / 1000);
    t = sign(__ENV.JWT_TENANT_SECRET, {
      sub, tid: T(ti).id, iat, exp: iat + 900, iss: ISSUER, aud: 'simple-core:tenant',
    });
    tokenCache[key] = t;
  }
  return t;
}

let ownerToken;
function platformToken() {
  if (!ownerToken) {
    const iat = Math.floor(Date.now() / 1000);
    ownerToken = sign(__ENV.JWT_PLATFORM_SECRET, {
      sub: meta.owner_id, iat, exp: iat + 900, iss: ISSUER, aud: 'simple-core:platform',
    });
  }
  return ownerToken;
}

function authParams(ti, op) {
  return {
    headers: { Authorization: `Bearer ${tenantToken(ti, pick(T(ti).tokens))}`, 'Content-Type': 'application/json' },
    tags: { op },
  };
}

http.setResponseCallback(http.expectedStatuses(200, 201, 202, 204));

// ------------------------------------------------------------------ operações

function getById(ti) {
  http.get(`${BASE}/v1/users/${pick(T(ti).targets)}`, authParams(ti, 'get'));
}

function list(ti) {
  http.get(`${BASE}/v1/users?limit=50&after=${pick(T(ti).targets)}`, authParams(ti, 'list'));
}

function patchName(ti) {
  const body = JSON.stringify({ name: `Load ${Math.floor(Math.random() * 1e9)}` });
  http.patch(`${BASE}/v1/users/${pick(T(ti).targets)}`, body, authParams(ti, 'patch'));
}

function deleteVictim() {
  // Índices pares no aquecimento, ímpares na medição: cada vítima é removida uma vez.
  const k = Math.floor(exec.scenario.iterationInTest / 100);
  const v = victims[2 * k + (exec.scenario.name === 'measure' ? 1 : 0)];
  if (!v) throw new Error('vítimas insuficientes');
  http.del(`${BASE}/v1/users/${v.id}`, null, authParams(v.t, 'delete'));
}

// 80% leitura, 15% listagem, 4% PATCH sem senha, 1% DELETE (determinístico por iteração).
function mix(choose) {
  const op = exec.scenario.iterationInTest % 100;
  if (op === 0) deleteVictim();
  else if (op <= 4) patchName(choose());
  else if (op <= 19) list(choose());
  else getById(choose());
}

const BIG = 0; // load-000: 1.000.000 de usuários
const uniform = () => Math.floor(Math.random() * tenants.length);

const actions = {
  get_by_id: () => getById(BIG),
  list: () => list(BIG),
  mix_single: () => mix(() => BIG),
  mix_all: () => mix(zipfTenant),
  pool_churn: () => getById(uniform()),
  noisy: () => getById(1),
  normal: () => getById(2 + Math.floor(Math.random() * (tenants.length - 2))),
  login: () => {
    const n = 1 + Math.floor(Math.random() * T(BIG).tokens.length);
    http.post(`${BASE}/v1/auth/login`, JSON.stringify({ email: `user${n}@load.test`, password: LOAD_PASSWORD }), {
      headers: { 'Content-Type': 'application/json', 'X-Tenant-Slug': T(BIG).slug },
      tags: { op: 'login' },
    });
  },
  create: () => {
    const email = `k6-${RUN_ID}-${exec.vu.idInTest}-${exec.vu.iterationInScenario}@load.test`;
    http.post(`${BASE}/v1/users`, JSON.stringify({ email, name: 'K6', password: LOAD_PASSWORD }), authParams(BIG, 'create'));
  },
  invalidator: () => {
    // Alterna as features de um perfil do tenant grande: invalida o cache do tenant inteiro.
    const features = exec.scenario.iterationInTest % 2 === 0 ? ['users.read', 'access.read'] : ['users.read'];
    http.put(`${BASE}/v1/profiles/${T(BIG).profiles[0]}/features`, JSON.stringify({ features }), {
      headers: { Authorization: `Bearer ${platformToken()}`, 'X-Tenant-Slug': T(BIG).slug, 'Content-Type': 'application/json' },
      tags: { op: 'invalidate' },
    });
  },
  provisioner: () => {
    const slug = `prov-${RUN_ID}-${exec.scenario.iterationInTest}`.toLowerCase().replace(/[^a-z0-9-]/g, '-');
    http.post(`${BASE}/v1/platform/tenants`, JSON.stringify({ slug, name: slug }), {
      headers: { Authorization: `Bearer ${platformToken()}`, 'Content-Type': 'application/json' },
      tags: { op: 'provision' },
    });
  },
};

export function run() {
  actions[__ENV.ACTION]();
}

// ------------------------------------------------------------------ cenários

function phased(action, rate, extra = {}, vus = VUS) {
  const base = {
    executor: 'constant-arrival-rate',
    rate,
    timeUnit: '1s',
    preAllocatedVUs: vus,
    maxVUs: vus * 2,
    exec: 'run',
  };
  const prefix = extra.prefix || '';
  return {
    [`${prefix}warmup`]: { ...base, duration: WARMUP, env: { ACTION: action }, tags: { phase: 'warmup', action, ...(extra.tags || {}) } },
    [`${prefix}measure`]: {
      ...base, startTime: WARMUP, duration: DURATION, env: { ACTION: action }, tags: { phase: 'measure', action, ...(extra.tags || {}) },
    },
  };
}

function background(action, rate) {
  return {
    [`bg_${action}`]: {
      executor: 'constant-arrival-rate', rate, timeUnit: '1s', preAllocatedVUs: 5, maxVUs: 20,
      duration: WARMUP === '0s' ? DURATION : `${parseInt(WARMUP, 10) + parseInt(DURATION, 10)}s`,
      exec: 'run', env: { ACTION: action }, tags: { phase: 'background', action },
    },
  };
}

const NOISY_RATE = parseInt(__ENV.NOISY_RATE || '6000', 10);
const definitions = {
  get_by_id: () => phased('get_by_id', RATE),
  list: () => phased('list', RATE),
  mix_single: () => phased('mix_single', RATE),
  mix_all: () => phased('mix_all', RATE),
  pool_churn: () => phased('pool_churn', RATE),
  noisy_neighbor: () => ({
    ...phased('normal', RATE, { tags: { group: 'normal' } }),
    ...phased('noisy', NOISY_RATE, { prefix: 'noisy_', tags: { group: 'noisy' } }),
  }),
  login: () => phased('login', RATE),
  create: () => phased('create', RATE),
  read_with_invalidations: () => ({ ...phased('get_by_id', RATE), ...background('invalidator', 2) }),
  read_with_provisioning: () => ({ ...phased('get_by_id', RATE), ...background('provisioner', 0.2) }),
};

const measureThresholds = {
  'http_req_duration{phase:measure}': ['p(99)<1000000'],
  'http_reqs{phase:measure}': ['count>=0'],
  'http_req_failed{phase:measure}': ['rate<=1'],
};
if (SCENARIO === 'noisy_neighbor') {
  for (const g of ['normal', 'noisy']) {
    measureThresholds[`http_req_duration{phase:measure,group:${g}}`] = ['p(99)<1000000'];
    measureThresholds[`http_reqs{phase:measure,group:${g}}`] = ['count>=0'];
    measureThresholds[`http_req_failed{phase:measure,group:${g}}`] = ['rate<=1'];
  }
}
for (const op of ['get', 'list', 'patch', 'delete', 'invalidate', 'provision']) {
  measureThresholds[`http_req_failed{op:${op}}`] = ['rate<=1'];
}

export const options = {
  scenarios: definitions[SCENARIO](),
  thresholds: measureThresholds,
  summaryTrendStats: ['avg', 'min', 'med', 'p(95)', 'p(99)', 'max'],
  discardResponseBodies: true,
  noConnectionReuse: false,
  insecureSkipTLSVerify: true,
};

// ------------------------------------------------------------------ resumo

function row(data, suffix, label) {
  const d = data.metrics[`http_req_duration{phase:measure${suffix}}`];
  const r = data.metrics[`http_reqs{phase:measure${suffix}}`];
  const f = data.metrics[`http_req_failed{phase:measure${suffix}}`];
  if (!d || !r) return '';
  const secs = parseInt(DURATION, 10);
  const ms = (x) => x.toFixed(2);
  return `| ${label} | ${RATE} | ${(r.values.count / secs).toFixed(0)} | ${ms(d.values.med)} | ${ms(d.values['p(95)'])} | ${ms(d.values['p(99)'])} | ${(100 * (f ? f.values.rate : 0)).toFixed(3)}% |`;
}

export function handleSummary(data) {
  const dropped = data.metrics.dropped_iterations ? data.metrics.dropped_iterations.values.count : 0;
  let md = '';
  if (SCENARIO === 'noisy_neighbor') {
    md += `${row(data, ',group:normal', `${SCENARIO} (normais)`)}\n`;
    md += `${row(data, ',group:noisy', `${SCENARIO} (barulhento, ${NOISY_RATE}/s)`)}\n`;
  } else {
    md += `${row(data, '', SCENARIO)}\n`;
  }
  const failures = Object.keys(data.metrics)
    .filter((k) => k.startsWith('http_req_failed{op:'))
    .map((k) => `${k}=${(100 * data.metrics[k].values.rate).toFixed(3)}%`)
    .join(' ');
  return {
    [`${OUT}/${SCENARIO}.json`]: JSON.stringify(data, null, 1),
    [`${OUT}/${SCENARIO}.md`]: md,
    stdout: `${md}dropped_iterations=${dropped} ${failures}\n`,
  };
}
