FROM ubuntu:devel AS builder

SHELL ["/bin/bash", "-c"]

COPY create_appdir.sh /tmp/

RUN \
    cd /tmp && \
# Allow unauthenticated repositories
    echo "Acquire::AllowInsecureRepositories true;" > /etc/apt/apt.conf.d/01-ci && \
    echo "APT::Get::AllowUnauthenticated true;" >> /etc/apt/apt.conf.d/01-ci && \
# Install MongoDB repository
    echo "deb http://repo.mongodb.org/apt/ubuntu jammy/mongodb-org/7.0 multiverse" > /etc/apt/sources.list.d/mongodb-org.list && \
# Update system
    apt-get update && \
    apt-get --option=Dpkg::Options::=--force-confdef -y upgrade && \
# Install MongoDB server
    apt-get install -y mongodb-org-server && \
# Create application directory
    /bin/bash create_appdir.sh

FROM scratch

COPY --from=builder /tmp/app/ /

USER 3500
ENTRYPOINT ["/usr/bin/mongod", "-f", "/data/mongod.conf"]
