#!/usr/bin/env python3
"""Check that the ledger's three regions agree with one another.

The five voters are named in several places that nothing else ties together
(docs/decisions.md, "Where the ledger's voters run"):

- infra/regions.bicep: the `ledger.internal` addresses (`voterAddresses`);
- infra/main.bicep, infra/second-region.bicep, infra/ledger-witness.bicep:
  the networks and the subnets those addresses must fall in;
- deploy/regions/*/peers.yaml: the internal load balancer fixed at each
  voter's address, and the pod behind it;
- deploy/k8s/settings.yaml: the peers, the witness and which voter is in
  which region.

A mismatch is quiet until the day it matters: a voter that answers at an
address its peers do not use, or a region the fail-back does not know, only
shows when a region is lost. So CI fails on any of them.

It also checks that the second region's AKS cluster is the home's, line for
line, so that the two regions do not drift apart.

Run from the repository root: python3 tools/check_regions.py
"""

import ipaddress
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def read(path):
    return (ROOT / path).read_text()


def resource_block(text, name):
    """The text of `resource <name> ... = { ... }`, up to its closing brace."""
    match = re.search(rf"^resource {name} [^\n]*\{{\n", text, re.M)
    if not match:
        return None
    end = text.index("\n}\n", match.end())
    return text[match.start() : end + 2]


def param_default(text, name):
    match = re.search(rf"^param {name} string = '([^']+)'", text, re.M)
    return match.group(1) if match else None


def voter_addresses(text):
    block = re.search(r"^param voterAddresses object = \{\n(.*?)^\}", text, re.M | re.S).group(1)
    return dict(re.findall(r"'([\w-]+)':\s*'([\d.]+)'", block))


def peer_services(text):
    """(pod name, fixed address) of each load balancer in a peers.yaml."""
    found = []
    for doc in text.split("\n---\n"):
        ip = re.search(r"azure-load-balancer-ipv4:\s*([\d.]+)", doc)
        pod = re.search(r"statefulset.kubernetes.io/pod-name:\s*([\w-]+)", doc)
        if ip and pod:
            found.append((pod.group(1), ip.group(1)))
    return found


def setting(text, name):
    match = re.search(rf'^\s*{name}:\s*"([^"]*)"', text, re.M)
    return match.group(1) if match else None


def check():
    errors = []
    main = read("infra/main.bicep")
    second = read("infra/second-region.bicep")
    witness = read("infra/ledger-witness.bicep")
    regions = read("infra/regions.bicep")
    settings = read("deploy/k8s/settings.yaml")

    # One cluster design, two regions.
    home_aks, second_aks = resource_block(main, "aks"), resource_block(second, "aks")
    if home_aks is None or second_aks is None:
        errors.append("the AKS cluster ('resource aks') is missing from main.bicep or second-region.bicep")
    elif home_aks != second_aks:
        errors.append("the AKS cluster in infra/second-region.bicep differs from infra/main.bicep; copy it across")

    # Each voter's address lies in its region's ledger-peers subnet (the
    # seventh /24 of the region's network) or, for the witness, in its own.
    home_net = ipaddress.ip_network(param_default(main, "vnetAddressSpace"))
    second_net = ipaddress.ip_network(param_default(second, "vnetAddressSpace"))
    witness_net = ipaddress.ip_network(param_default(witness, "vnetAddressSpace"))
    subnets = {
        "australiaeast": list(home_net.subnets(new_prefix=24))[6],
        "australiasoutheast": list(second_net.subnets(new_prefix=24))[6],
        "australiacentral": list(witness_net.subnets(new_prefix=26))[0],
    }
    for net_a, net_b in [(home_net, second_net), (home_net, witness_net), (second_net, witness_net)]:
        if net_a.overlaps(net_b):
            errors.append(f"the regions' networks {net_a} and {net_b} overlap and cannot be peered")

    addresses = voter_addresses(regions)
    if addresses.get("ic-witness") != param_default(witness, "witnessAddress"):
        errors.append(
            f"infra/regions.bicep names the witness at {addresses.get('ic-witness')}, "
            f"infra/ledger-witness.bicep gives it {param_default(witness, 'witnessAddress')}"
        )

    # The settings: who the peers are, which is the witness, and where each is.
    peers = dict(entry.split("=", 1) for entry in setting(settings, "IC_NODE_PEERS").split(","))
    witnesses = set(setting(settings, "IC_WITNESS_NODES").split(","))
    placed = {}
    for region in setting(settings, "IC_REGION_NODES").split(";"):
        name, ids = region.split("=", 1)
        for node in ids.split(","):
            placed[node] = name
    homes = setting(settings, "IC_LEADER_REGIONS").split(",")
    nodes = set(setting(settings, "IC_NODES").split(","))

    if set(peers) != set(placed):
        errors.append(f"IC_NODE_PEERS names nodes {sorted(peers)} but IC_REGION_NODES places {sorted(placed)}")
    if set(peers.values()) != nodes:
        errors.append("IC_NODES and IC_NODE_PEERS name different addresses")
    for home in homes:
        if home not in placed.values():
            errors.append(f"IC_LEADER_REGIONS names {home}, which has no voters in IC_REGION_NODES")
        if any(placed[n] == home for n in witnesses):
            errors.append(f"the witness is placed in {home}, a region that leads")
    if len(witnesses) * 2 >= len(peers):
        errors.append("there must be fewer witnesses than half the voters")

    # The overlays: one fixed load balancer per data voter, in its region.
    overlays = {}
    for region in ("australiaeast", "australiasoutheast"):
        path = f"deploy/regions/{region}/peers.yaml"
        for pod, ip in peer_services(read(path)):
            overlays[pod] = (region, ip)

    for node, addr in sorted(peers.items()):
        host = addr.split(":")[0]
        name = host.removesuffix(".ledger.internal")
        if not host.endswith(".ledger.internal"):
            errors.append(f"voter {node} is at {host}, outside the ledger.internal zone")
            continue
        ip = addresses.get(name)
        if ip is None:
            errors.append(f"voter {node} ({name}) has no address in infra/regions.bicep")
            continue
        region = placed.get(node)
        if region in subnets and ipaddress.ip_address(ip) not in subnets[region]:
            errors.append(f"{name} at {ip} is not in {region}'s ledger subnet {subnets[region]}")
        if node in witnesses:
            continue
        if name != f"ic-node-{node}":
            errors.append(f"data voter {node} is named {name}; its pod is ic-node-{node}")
        if name not in overlays:
            errors.append(f"no load balancer in deploy/regions for {name}")
        elif overlays[name] != (region, ip):
            errors.append(f"deploy/regions puts {name} at {overlays[name]}, the settings and zone at {(region, ip)}")
    for stray in sorted(set(overlays) - {f"ic-node-{n}" for n in peers}):
        errors.append(f"deploy/regions has a load balancer for {stray}, which is not a voter")
    for name in sorted(set(addresses) - {a.split(':')[0].removesuffix('.ledger.internal') for a in peers.values()}):
        errors.append(f"infra/regions.bicep names {name}, which is not a voter")

    # Each overlay's StatefulSet numbers its pods from its first voter.
    first = {}
    for node, region in placed.items():
        if node not in witnesses:
            first[region] = min(first.get(region, int(node)), int(node))
    home_start = re.search(r"ordinals:\s*\n\s*start:\s*(\d+)", read("deploy/k8s/node.yaml"))
    second_start = re.search(r"path: /spec/ordinals/start\s*\n\s*value:\s*(\d+)", read("deploy/regions/australiasoutheast/kustomization.yaml"))
    if not home_start or int(home_start.group(1)) != first.get("australiaeast"):
        errors.append("deploy/k8s/node.yaml must number the home's pods from its first voter")
    if not second_start or int(second_start.group(1)) != first.get("australiasoutheast"):
        errors.append("deploy/regions/australiasoutheast must number its pods from its first voter")

    return errors


def main():
    errors = check()
    for error in errors:
        print(f"check_regions: {error}", file=sys.stderr)
    if errors:
        sys.exit(1)
    print("check_regions: the three regions agree")


if __name__ == "__main__":
    main()
