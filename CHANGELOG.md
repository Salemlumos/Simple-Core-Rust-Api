# Changelog

## 0.1.0 (2026-11-11)


### Features

* API multi-tenant de usuários com controle de acesso ([a7f693a](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/a7f693aa20eb6eb67222102c89cf58b86fa2d03d))
* documentação OpenAPI 3.1 (utoipa) com Swagger UI em /docs ([0e5a8ef](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/0e5a8ef4a96ea8a17fabcce294d74af2cc4925b1))
* script new-project.sh para iniciar um sistema novo a partir desta base ([b6c7052](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/b6c7052186605055c43fee3ff7c7799df944d26a))


### Bug Fixes

* **new-project:** roda cargo fmt depois de renomear ([cab5d26](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/cab5d2645ced89f7c3aa32346737b328b2c134a4))


### Performance

* desliga test_before_acquire nos pools; adiciona scripts de carga ([e944321](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/e944321ce4f0a917bc9be577203c554c9609231f))
* pools 10x110; docs: README com execução, decisões e resultados de carga ([410a020](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/410a020d76103cc82deb21233200fe5f88c6487b))
* pools 3x120, tcp_tw_reuse, mimalloc por padrão; k6 com tokens compartilhados ([d51ce74](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/d51ce74efc0f2c763850393c5119f141895f76db))


### Documentation

* como iniciar um projeto novo com scripts/new-project.sh ([694e5d2](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/694e5d2b4566eb91d0cb4132196cac1962a86705))
* README em inglês (README.en.md) com link entre os idiomas ([f13b158](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/f13b1584c86d9dcc596dcf810f38b97233e1a9bc))
* README principal em inglês; português em README.pt-BR.md ([58b8cc7](https://github.com/Salemlumos/Simple-Core-Rust-Api/commit/58b8cc741195455267281dde0f115f2b9de019d9))
