/*
 * Copyright 2024 RisingWave Labs
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

import {
  Box,
  Button,
  Flex,
  FormControl,
  FormLabel,
  HStack,
  Input,
  Select,
  Table,
  TableContainer,
  Tbody,
  Td,
  Text,
  Tr,
  VStack,
} from "@chakra-ui/react"
import * as d3 from "d3"
import { dagStratify } from "d3-dag"
import _ from "lodash"
import Head from "next/head"
import { parseAsInteger, useQueryState } from "nuqs"
import { Fragment, useCallback, useEffect, useMemo, useState } from "react"
import FragmentDependencyGraph from "../components/FragmentDependencyGraph"
import FragmentGraph from "../components/FragmentGraph"
import Title from "../components/Title"
import useErrorToast from "../hook/useErrorToast"
import useFetch from "../lib/api/fetch"
import {
  calculateBPRate,
  calculateCumulativeBp,
  fetchEmbeddedBackPressure,
  fetchPrometheusBackPressure,
} from "../lib/api/metric"
import {
  getFragmentsByJobId,
  getRelationIdInfos,
  getStreamingJobs,
} from "../lib/api/streaming"
import { FragmentBox } from "../lib/layout"
import { TableFragments, TableFragments_Fragment } from "../proto/gen/meta"
import { BackPressureInfo, FragmentStats } from "../proto/gen/monitor_service"
import { Dispatcher, MergeNode, StreamNode } from "../proto/gen/stream_plan"

interface DispatcherNode {
  [actorId: number]: Dispatcher[]
  fragment: TableFragments_Fragment
}

// Refresh interval (ms) for back pressure stats
const INTERVAL_MS = 5000

/** Associated data of each plan node in the fragment graph, including the dispatchers. */
export interface PlanNodeDatum {
  name: string
  children?: PlanNodeDatum[]
  operatorId: string | number
  node: StreamNode | DispatcherNode
  actorIds?: string[]
}

function buildPlanNodeDependency(
  fragment: TableFragments_Fragment
): d3.HierarchyNode<PlanNodeDatum> {
  const firstActor = fragment.actors[0]

  const hierarchyActorNode = (node: StreamNode): PlanNodeDatum => {
    return {
      name: node.nodeBody?.$case?.toString() || "unknown",
      children: (node.input || []).map(hierarchyActorNode),
      operatorId: node.operatorId,
      node,
    }
  }

  let dispatcherName: string

  if (firstActor.dispatcher.length > 0) {
    const firstDispatcherName = _.camelCase(
      firstActor.dispatcher[0].type.replace(/^DISPATCHER_TYPE_/, "")
    )
    if (firstActor.dispatcher.length > 1) {
      if (
        firstActor.dispatcher.every(
          (d) => d.type === firstActor.dispatcher[0].type
        )
      ) {
        dispatcherName = `${firstDispatcherName}Dispatchers`
      } else {
        dispatcherName = "multipleDispatchers"
      }
    } else {
      dispatcherName = `${firstDispatcherName}Dispatcher`
    }
  } else {
    dispatcherName = "noDispatcher"
  }

  let dispatcherNode = fragment.actors.reduce((obj, actor) => {
    obj[actor.actorId] = actor.dispatcher
    return obj
  }, {} as DispatcherNode)
  dispatcherNode.fragment = {
    ...fragment,
    actors: [],
  }

  return d3.hierarchy({
    name: dispatcherName,
    actorIds: fragment.actors.map((a) => a.actorId.toString()),
    children: firstActor.nodes ? [hierarchyActorNode(firstActor.nodes)] : [],
    operatorId: "dispatcher",
    node: dispatcherNode,
  })
}

function findMergeNodes(root: StreamNode): MergeNode[] {
  let mergeNodes = new Set<MergeNode>()

  const findMergeNodesRecursive = (node: StreamNode) => {
    if (node.nodeBody?.$case === "merge") {
      mergeNodes.add(node.nodeBody.merge)
    }
    for (const child of node.input || []) {
      findMergeNodesRecursive(child)
    }
  }

  findMergeNodesRecursive(root)
  return Array.from(mergeNodes)
}

function buildFragmentDependencyAsEdges(
  fragments: TableFragments
): FragmentBox[] {
  const nodes: FragmentBox[] = []
  const actorToFragmentMapping = new Map<number, number>()
  for (const fragmentId in fragments.fragments) {
    const fragment = fragments.fragments[fragmentId]
    for (const actor of fragment.actors) {
      actorToFragmentMapping.set(actor.actorId, actor.fragmentId)
    }
  }
  for (const id in fragments.fragments) {
    const fragment = fragments.fragments[id]
    const parentIds = new Set<number>()
    const externalParentIds = new Set<number>()

    for (const actor of fragment.actors) {
      for (const upstreamActorId of actor.upstreamActorId) {
        const upstreamFragmentId = actorToFragmentMapping.get(upstreamActorId)
        if (upstreamFragmentId) {
          parentIds.add(upstreamFragmentId)
        } else {
          for (const m of findMergeNodes(actor.nodes!)) {
            externalParentIds.add(m.upstreamFragmentId)
          }
        }
      }
    }
    nodes.push({
      id: fragment.fragmentId.toString(),
      name: `Fragment ${fragment.fragmentId}`,
      parentIds: Array.from(parentIds).map((x) => x.toString()),
      externalParentIds: Array.from(externalParentIds).map((x) => x.toString()),
      width: 0,
      height: 0,
      order: fragment.fragmentId,
      fragment,
    })
  }
  return nodes
}

const SIDEBAR_WIDTH = 225

type BackPressureDataSource = "Embedded" | "Prometheus"
const backPressureDataSources: BackPressureDataSource[] = [
  "Embedded",
  "Prometheus",
]

// The state of the embedded back pressure metrics.
// The metrics from previous fetch are stored here to calculate the rate.
interface EmbeddedBackPressureInfo {
  previous: BackPressureInfo[]
  current: BackPressureInfo[]
  totalBackpressureNs: BackPressureInfo[]
  totalDurationNs: number
}

export default function Streaming() {
  const { response: streamingJobList } = useFetch(getStreamingJobs)
  const { response: relationIdInfos } = useFetch(getRelationIdInfos)

  const [jobId, setJobId] = useQueryState("id", parseAsInteger)
  const [selectedFragmentId, setSelectedFragmentId] = useState<number>()
  const [tableFragments, setTableFragments] = useState<TableFragments>()

  const job = useMemo(
    () => streamingJobList?.find((j) => j.id === jobId),
    [streamingJobList, jobId]
  )

  const toast = useErrorToast()

  useEffect(() => {
    if (jobId) {
      setTableFragments(undefined)
      getFragmentsByJobId(jobId).then((tf) => {
        setTableFragments(tf)
      })
    }
  }, [jobId])

  const fragmentDependencyCallback = useCallback(() => {
    if (tableFragments) {
      const fragmentDep = buildFragmentDependencyAsEdges(tableFragments)
      return {
        fragments: tableFragments,
        fragmentDep,
        fragmentDepDag: dagStratify()(fragmentDep),
      }
    }
  }, [tableFragments])

  useEffect(() => {
    if (streamingJobList) {
      if (!jobId) {
        if (streamingJobList.length > 0) {
          setJobId(streamingJobList[0].id)
        }
      }
    }
  }, [jobId, streamingJobList, setJobId])

  // The table fragments of the selected fragment id
  const fragmentDependency = fragmentDependencyCallback()?.fragmentDep
  const fragmentDependencyDag = fragmentDependencyCallback()?.fragmentDepDag
  const fragments = fragmentDependencyCallback()?.fragments

  const planNodeDependenciesCallback = useCallback(() => {
    const fragments_ = fragments?.fragments
    if (fragments_) {
      const planNodeDependencies = new Map<
        string,
        d3.HierarchyNode<PlanNodeDatum>
      >()
      for (const fragmentId in fragments_) {
        const fragment = fragments_[fragmentId]
        const dep = buildPlanNodeDependency(fragment)
        planNodeDependencies.set(fragmentId, dep)
      }
      return planNodeDependencies
    }
    return undefined
  }, [fragments?.fragments])

  const planNodeDependencies = planNodeDependenciesCallback()

  const [searchActorId, setSearchActorId] = useState<string>("")
  const [searchFragId, setSearchFragId] = useState<string>("")

  const handleSearchFragment = () => {
    const searchFragIdInt = parseInt(searchFragId)
    if (relationIdInfos) {
      let map = relationIdInfos.map
      for (const relationId in map) {
        const fragmentIdToRelationId = map[relationId].map
        for (const fragmentId in fragmentIdToRelationId) {
          if (parseInt(fragmentId) == searchFragIdInt) {
            setJobId(parseInt(relationId))
            setSelectedFragmentId(searchFragIdInt)
            return
          }
        }
      }
    }
    toast(new Error(`Fragment ${searchFragIdInt} not found`))
  }

  const handleSearchActor = () => {
    const searchActorIdInt = parseInt(searchActorId)
    if (relationIdInfos) {
      let map = relationIdInfos.map
      for (const relationId in map) {
        const fragmentIdToRelationId = map[relationId].map
        for (const fragmentId in fragmentIdToRelationId) {
          let actorIds = fragmentIdToRelationId[fragmentId].ids
          if (actorIds.includes(searchActorIdInt)) {
            setJobId(parseInt(relationId))
            setSelectedFragmentId(parseInt(fragmentId))
            return
          }
        }
      }
    }
    toast(new Error(`Actor ${searchActorIdInt} not found`))
  }

  const [backPressureDataSource, setBackPressureDataSource] =
    useState<BackPressureDataSource>("Embedded")

  // Periodically fetch Prometheus back-pressure from Meta node
  const { response: promethusMetrics } = useFetch(
    fetchPrometheusBackPressure,
    INTERVAL_MS,
    backPressureDataSource === "Prometheus"
  )

  // Periodically fetch embedded back-pressure from Meta node
  // Didn't call `useFetch()` because the `setState` way is special.
  const [embeddedBackPressureInfo, setEmbeddedBackPressureInfo] =
    useState<EmbeddedBackPressureInfo>()
  const [fragmentStats, setFragmentStats] = useState<{
    [key: number]: FragmentStats
  }>()

  useEffect(() => {
    if (backPressureDataSource === "Embedded") {
      function refresh() {
        fetchEmbeddedBackPressure().then(
          (response) => {
            console.log(response)
            let newBP =
              response.backPressureInfos?.map(BackPressureInfo.fromJSON) ?? []
            setEmbeddedBackPressureInfo((prev) =>
              prev
                ? {
                    previous: prev.current,
                    current: newBP,
                    totalBackpressureNs: calculateCumulativeBp(
                      prev.totalBackpressureNs,
                      prev.current,
                      newBP
                    ),
                    totalDurationNs:
                      prev.totalDurationNs + INTERVAL_MS * 1000 * 1000,
                  }
                : {
                    previous: newBP, // Use current value to show zero rate, but it's fine
                    current: newBP,
                    totalBackpressureNs: [],
                    totalDurationNs: 0,
                  }
            )
            setFragmentStats(response.fragmentStats)
          },
          (e) => {
            console.error(e)
            toast(e, "error")
          }
        )
      }
      refresh()
      const interval = setInterval(refresh, INTERVAL_MS)
      return () => {
        clearInterval(interval)
      }
    }
  }, [backPressureDataSource, toast])

  const backPressures = useMemo(() => {
    if (promethusMetrics || embeddedBackPressureInfo) {
      let map = new Map()

      if (backPressureDataSource === "Embedded" && embeddedBackPressureInfo) {
        const metrics = calculateBPRate(
          embeddedBackPressureInfo.totalBackpressureNs,
          embeddedBackPressureInfo.totalDurationNs
        )
        for (const m of metrics.outputBufferBlockingDuration) {
          map.set(
            `${m.metric.fragmentId}_${m.metric.downstreamFragmentId}`,
            m.sample[0].value
          )
        }
      } else if (backPressureDataSource === "Prometheus" && promethusMetrics) {
        for (const m of promethusMetrics.outputBufferBlockingDuration) {
          if (m.sample.length > 0) {
            // Note: We issue an instant query to Prometheus to get the most recent value.
            // So there should be only one sample here.
            //
            // Due to https://github.com/risingwavelabs/risingwave/issues/15280, it's still
            // possible that an old version of meta service returns a range-query result.
            // So we take the one with the latest timestamp here.
            const value = _(m.sample).maxBy((s) => s.timestamp)!.value * 100
            map.set(
              `${m.metric.fragment_id}_${m.metric.downstream_fragment_id}`,
              value
            )
          }
        }
      }
      return map
    }
  }, [backPressureDataSource, promethusMetrics, embeddedBackPressureInfo])

  const retVal = (
    <Flex p={3} height="calc(100vh - 20px)" flexDirection="column">
      <Title>Fragment Graph</Title>
      <Flex flexDirection="row" height="full" width="full">
        <VStack
          mr={3}
          spacing={3}
          alignItems="flex-start"
          width={SIDEBAR_WIDTH}
          height="full"
        >
          <FormControl>
            <FormLabel>Streaming Jobs</FormLabel>
            <Input
              list="relationList"
              spellCheck={false}
              onChange={(event) => {
                const id = streamingJobList?.find(
                  (x) => x.name == event.target.value
                )?.id
                if (id) {
                  setJobId(id)
                }
              }}
              placeholder="Search..."
              mb={2}
            ></Input>
            <datalist id="relationList">
              {streamingJobList &&
                streamingJobList.map((r) => (
                  <option value={r.name} key={r.id}>
                    ({r.id}) {r.name}
                  </option>
                ))}
            </datalist>
            <Select
              value={jobId ?? undefined}
              onChange={(event) => setJobId(parseInt(event.target.value))}
            >
              {streamingJobList &&
                streamingJobList.map((r) => (
                  <option value={r.id} key={r.name}>
                    ({r.id}) {r.name}
                  </option>
                ))}
            </Select>
          </FormControl>
          {job && (
            <FormControl>
              <FormLabel>Information</FormLabel>
              <TableContainer>
                <Table size="sm">
                  <Tbody>
                    <Tr>
                      <Td fontWeight="medium">Type</Td>
                      <Td isNumeric>{job.type}</Td>
                    </Tr>
                    <Tr>
                      <Td fontWeight="medium">Status</Td>
                      <Td isNumeric>{job.jobStatus}</Td>
                    </Tr>
                    <Tr>
                      <Td fontWeight="medium">Parallelism</Td>
                      <Td isNumeric>{job.parallelism}</Td>
                    </Tr>
                    <Tr>
                      <Td fontWeight="medium" paddingEnd={0}>
                        Max Parallelism
                      </Td>
                      <Td isNumeric>{job.maxParallelism}</Td>
                    </Tr>
                  </Tbody>
                </Table>
              </TableContainer>
            </FormControl>
          )}
          <FormControl>
            <FormLabel>Goto</FormLabel>
            <VStack spacing={2}>
              <HStack>
                <Input
                  placeholder="Fragment Id"
                  value={searchFragId}
                  onChange={(event) => setSearchFragId(event.target.value)}
                ></Input>
                <Button onClick={(_) => handleSearchFragment()}>Go</Button>
              </HStack>
              <HStack>
                <Input
                  placeholder="Actor Id"
                  value={searchActorId}
                  onChange={(event) => setSearchActorId(event.target.value)}
                ></Input>
                <Button onClick={(_) => handleSearchActor()}>Go</Button>
              </HStack>
            </VStack>
          </FormControl>
          <FormControl>
            <FormLabel>Back Pressure Data Source</FormLabel>
            <Select
              value={backPressureDataSource}
              onChange={(event) =>
                setBackPressureDataSource(
                  event.target.value as BackPressureDataSource
                )
              }
              defaultValue="Embedded"
            >
              <option value="Embedded" key="Embedded">
                Embedded (5 secs)
              </option>
              <option value="Prometheus" key="Prometheus">
                Prometheus (1 min)
              </option>
            </Select>
          </FormControl>
          <Flex height="full" width="full" flexDirection="column">
            <Text fontWeight="semibold">Fragments</Text>
            {fragmentDependencyDag && (
              <Box flex="1" overflowY="scroll">
                <FragmentDependencyGraph
                  svgWidth={SIDEBAR_WIDTH}
                  fragmentDependency={fragmentDependencyDag}
                  onSelectedIdChange={(id) =>
                    setSelectedFragmentId(parseInt(id))
                  }
                  selectedId={selectedFragmentId?.toString()}
                />
              </Box>
            )}
          </Flex>
        </VStack>
        <Box
          flex={1}
          height="full"
          ml={3}
          overflowX="scroll"
          overflowY="scroll"
        >
          <Text fontWeight="semibold">Fragment Graph</Text>
          {planNodeDependencies && fragmentDependency && (
            <FragmentGraph
              selectedFragmentId={selectedFragmentId?.toString()}
              fragmentDependency={fragmentDependency}
              planNodeDependencies={planNodeDependencies}
              backPressures={backPressures}
              fragmentStats={fragmentStats}
            />
          )}
        </Box>
      </Flex>
    </Flex>
  )

  return (
    <Fragment>
      <Head>
        <title>Streaming Fragments</title>
      </Head>
      {retVal}
    </Fragment>
  )
}
