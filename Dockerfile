FROM debian:sid-slim AS builder

SHELL ["/bin/bash", "-c"]

COPY create_appdir.sh /tmp/

RUN \
    cd /tmp && \
# Enable experimental repository
    sed -Ei 's/sid/sid experimental/; s/main/main contrib/;' /etc/apt/sources.list.d/debian.sources && \
# Allow unauthenticated repositories
    echo "Acquire::AllowInsecureRepositories true;" > /etc/apt/apt.conf.d/01-ci && \
    echo "APT::Get::AllowUnauthenticated true;" >> /etc/apt/apt.conf.d/01-ci && \
# Install MongoDB repository
    echo "deb http://repo.mongodb.org/apt/debian bullseye/mongodb-org/6.0 main" > /etc/apt/sources.list.d/mongodb-org.list && \
# Update system
    apt-get update && \
    apt-get -t experimental --option=Dpkg::Options::=--force-confdef -y upgrade && \
# Install MongoDB server
    apt-get -t experimental install -y mongodb-org-server && \
# Create application directory
    /bin/bash create_appdir.sh

FROM scratch

COPY --from=builder /tmp/app/ /

USER 3500
ENTRYPOINT ["/usr/bin/mongod", "-f", "/data/mongod.conf"]
