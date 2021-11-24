.PHONY: build-images push-images
INTRODUCER_VERSION=v1
WRAFT_VERSION=v1

build-images:
	docker build . -f Dockerfile.wraft -t harbor.eevans.me/library/wraft:$(WRAFT_VERSION)
	docker build . -f Dockerfile.webrtc-introducer -t harbor.eevans.me/library/webrtc-introducer:$(INTRODUCER_VERSION)

push-images: build-images
	docker push harbor.eevans.me/library/wraft:$(WRAFT_VERSION)
	docker push harbor.eevans.me/library/webrtc-introducer:$(INTRODUCER_VERSION)
