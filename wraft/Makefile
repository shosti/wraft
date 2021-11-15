.PHONY: deploy build wasm js

build: wasm js

wasm:
	wasm-pack build --release

js:
	cd www && npm i && npm run build

deploy: build
	for i in 0 1 2; do gsutil -h "Cache-Control:no-cache, max-age=0" rsync www/dist/ gs://wraft$$i.eevans.co/; done
