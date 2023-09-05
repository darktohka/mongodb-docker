# MongoDB Docker (slim)

A very slim Docker image, containing the absolute minimum amount of libraries required to run MongoDB.

This image is based on the Debian Unstable (Debian Sid) image.

## Usage: Create config, directories and run MongoDB

By default, MongoDB listens on 27017. Be sure to open this port in your firewall.

You may have to set up multiple volumes:

- A volume at `/data/db` to keep your database
- A volume at `/data/logs` to keep your logs
- A volume at `/data/mongod.conf` for your config
- A volume at `/data/mongod.pem` for your SSL certificates

Make sure your data directories are owned by user ID `3500`!

You may find a default configuration in this repository. Copy the `mongod.conf` file from this repository and change it according to your needs.

### Docker example

```
sudo docker image pull darktohka/mongodb-docker:7.0
sudo docker run --name mongodb -p 27017:27017 -v $(pwd)/mongod.conf:/data/mongod.conf:ro -v $(pwd)/mongod.pem:/data/mongod.pem:ro -v $(pwd)/data/logs:/data/logs:rw -v $(pwd)/data/db:/data/db:rw darktohka/mongodb-docker:7.0
```

### Docker-Compose example

First, run the following commands:

```
mkdir -p data/logs data/db
chown -R 3500:3500 data
```

Then, copy the `mongod.conf` file from this repository and change it according to your needs.

Then create the following `docker-compose.yml` file:

```
services:
  mongodb:
    image: darktohka/mongodb-docker:7.0
    ports:
    - "27017:27017"
    networks:
    - "mongodb"
    volumes:
    - "./mongod.conf:/data/mongod.conf:ro"
    - "./mongod.pem:/data/mongod.pem:ro"
    - "./data/logs:/data/logs:rw"
    - "./data/db:/data/db:rw"

networks:
  mongodb:
```
