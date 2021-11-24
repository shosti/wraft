.PHONY: build-image push-image
INTRODUCER_VERSION=v1

build-image:
	docker build . -t harbor.eevans.me/library/webrtc-introducer:$(INTRODUCER_VERSION)

push-image: build-image
	docker push harbor.eevans.me/library/webrtc-introducer:$(INTRODUCER_VERSION)
