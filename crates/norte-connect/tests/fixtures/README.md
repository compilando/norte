# Fixtures de test de norte-connect

## `id_rsa_test`

Clave privada RSA **generada exclusivamente para tests** (nunca usada en
ningún sistema real, sin valor como secreto). Existe para probar que el
conector **rechaza** claves RSA (`ConnectError::KeyUnsupported`, ADR 0015 E,
issue #36 / RUSTSEC-2023-0071): no se puede generar en tiempo de test porque
el keygen RSA en modo debug tarda decenas de segundos.

Si un escáner de secretos (gitleaks, GitHub secret scanning) la señala,
es un falso positivo esperado: añadidla a su allowlist.
