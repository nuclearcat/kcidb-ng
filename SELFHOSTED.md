# KCIDB-ng Self-Hosted Guide

This guide provides instructions for setting up and managing a self-hosted instance of KCIDB-ng.

## Overview

The self-hosted setup uses Docker Compose to run the necessary services in a containerized environment. The main components are:

-   **kcidb-rest-rs**: The Rust-based REST API for submitting data.
-   **ingester**: A Python service that processes and validates submissions.
-   **logspec-worker**: A Python service for analyzing log files.
-   **PostgreSQL**: The database for storing KCIDB data.

## Quick Start

The easiest way to get started is by using the `self-hosted.sh` script.

1.  **Run the services**:
    ```bash
    ./self-hosted.sh run
    ```
    This command will:
    -   Create a `.env` file with default values if it doesn't exist.
    -   Build and start all the Docker containers in detached mode.
    -   Create a default `logspec_worker.yaml` if one is not present.

2.  **Stop the services**:
    ```bash
    ./self-hosted.sh down
    ```
    This stops and removes the Docker containers.

3.  **Clean the environment**:
    ```bash
    ./self-hosted.sh clean
    ```
    This will stop and remove all containers, volumes, and networks associated with the project. **This will delete all data.**

## Manual Setup

If you prefer to set up the environment manually, you will need to:

1.  **Create a `.env` file**:
    Copy the `.env-example` file to `.env` and customize the values. The `JWT_SECRET` is a critical component for securing your instance.

2.  **Run Docker Compose**:
    ```bash
    docker compose --profile=self-hosted up -d --build
    ```

## Directory Structure

-   `spool/`: This directory is used by the `ingester` to process submissions.
    -   `failed/`: Submissions that fail validation are moved here.
    -   `archive/`: Successfully processed submissions are archived here.
-   `state/`: Contains databases that track the state of processed builds and tests.
-   `cache/`: Caches downloaded log files for the `logspec-worker`.
-   `config/`: Holds configuration files, such as `logspec_worker.yaml`.
-   `db/`: The PostgreSQL data directory.

## Generating a JWT Token

To interact with the API, you will need a JWT token. You can generate one with the following command:

```bash
kcidb-restd-rs/tools/jwt_rest.py --secret YOUR_SECRET --origin YOUR_ORIGIN
```

Replace `YOUR_SECRET` with the `JWT_SECRET` from your `.env` file and `YOUR_ORIGIN` with a descriptive name for the token's origin.

## Disabling Authentication

For development or testing in an isolated environment, you can disable JWT authentication by uncommenting the following lines in `docker-compose.yaml`:

```yaml
#    command: ["/usr/local/bin/kcidb-restd-rs","-j",""]
```

## Viewing Logs

You can view the logs for each service using the following commands:

```bash
docker logs kcidb-rest
docker logs ingester
docker logs logspec-worker
docker logs postgres
```

## Accessing the Database

You can connect to the PostgreSQL database with the following command:

```bash
docker exec -it postgres psql -U kcidb_editor -d kcidb
```
