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

import { theme } from "@chakra-ui/react"
import * as d3 from "d3"
import { useCallback, useEffect, useRef } from "react"
import {
  Relation,
  relationIsStreamingJob,
  relationType,
  relationTypeTitleCase,
} from "../lib/api/streaming"
import {
  Edge,
  Enter,
  Position,
  RelationPoint,
  RelationPointPosition,
  flipLayoutRelation,
  generateRelationEdges,
} from "../lib/layout"
import { RelationStats } from "../proto/gen/monitor_service"
import { CatalogModal, useCatalogModal } from "./CatalogModal"
import {
  backPressureColor,
  backPressureWidth,
  epochToUnixMillis,
  latencyToColor,
} from "./utils/backPressure"

function boundBox(
  relationPosition: RelationPointPosition[],
  nodeRadius: number
): {
  width: number
  height: number
} {
  let width = 0
  let height = 0
  for (const { x, y } of relationPosition) {
    width = Math.max(width, x + nodeRadius)
    height = Math.max(height, y + nodeRadius)
  }
  return { width, height }
}

const layerMargin = 50
const rowMargin = 50
export const nodeRadius = 12
const layoutMargin = 50

export default function RelationGraph({
  nodes,
  selectedId,
  setSelectedId,
  backPressures,
  relationStats,
}: {
  nodes: RelationPoint[]
  selectedId: string | undefined
  setSelectedId: (id: string) => void
  backPressures?: Map<string, number> // relationId-relationId->back_pressure_rate})
  relationStats: { [relationId: number]: RelationStats } | undefined
}) {
  const [modalData, setModalId] = useCatalogModal(nodes.map((n) => n.relation))

  const svgRef = useRef<SVGSVGElement>(null)

  const layoutMapCallback = useCallback(() => {
    const layoutMap = flipLayoutRelation(
      nodes,
      layerMargin,
      rowMargin,
      nodeRadius
    ).map(
      ({ x, y, ...data }) =>
        ({
          x: x + layoutMargin,
          y: y + layoutMargin,
          ...data,
        } as RelationPointPosition)
    )
    const links = generateRelationEdges(layoutMap)
    const { width, height } = boundBox(layoutMap, nodeRadius)
    return {
      layoutMap,
      links,
      width: width + rowMargin + layoutMargin * 2,
      height: height + layerMargin + layoutMargin * 2,
    }
  }, [nodes])

  const { layoutMap, width, height, links } = layoutMapCallback()

  useEffect(() => {
    const now_ms = Date.now()
    const svgNode = svgRef.current
    const svgSelection = d3.select(svgNode)

    const curveStyle = d3.curveMonotoneY

    const line = d3
      .line<Position>()
      .curve(curveStyle)
      .x(({ x }) => x)
      .y(({ y }) => y)

    const edgeSelection = svgSelection
      .select(".edges")
      .selectAll<SVGPathElement, null>(".edge")
      .data(links)
    type EdgeSelection = typeof edgeSelection

    const isSelected = (id: string) => id === selectedId

    const applyEdge = (sel: EdgeSelection) => {
      const color = (d: Edge) => {
        if (backPressures) {
          let value = backPressures.get(`${d.target}_${d.source}`)
          if (value) {
            return backPressureColor(value)
          }
        }

        return theme.colors.gray["300"]
      }

      const width = (d: Edge) => {
        if (backPressures) {
          let value = backPressures.get(`${d.target}_${d.source}`)
          if (value) {
            return backPressureWidth(value, 15)
          }
        }

        return 2
      }

      sel
        .attr("d", ({ points }) => line(points))
        .attr("fill", "none")
        .attr("stroke-width", width)
        .attr("stroke", color)
        .attr("opacity", (d) =>
          isSelected(d.source) || isSelected(d.target) ? 1 : 0.5
        )

      sel
        .on("mouseover", (event, d) => {
          // Remove existing tooltip if any
          d3.selectAll(".tooltip").remove()

          if (backPressures) {
            const value = backPressures.get(`${d.target}_${d.source}`)
            if (value) {
              // Create new tooltip
              d3.select("body")
                .append("div")
                .attr("class", "tooltip")
                .style("position", "absolute")
                .style("background", "white")
                .style("padding", "10px")
                .style("border", "1px solid #ddd")
                .style("border-radius", "4px")
                .style("pointer-events", "none")
                .style("left", event.pageX + 10 + "px")
                .style("top", event.pageY + 10 + "px")
                .style("font-size", "12px")
                .html(`BP: ${value.toFixed(2)}%`)
            }
          }
        })
        .on("mousemove", (event) => {
          d3.select(".tooltip")
            .style("left", event.pageX + 10 + "px")
            .style("top", event.pageY + 10 + "px")
        })
        .on("mouseout", () => {
          d3.selectAll(".tooltip").remove()
        })

      return sel
    }

    const createEdge = (sel: Enter<EdgeSelection>) =>
      sel.append("path").attr("class", "edge").call(applyEdge)
    edgeSelection.exit().remove()
    edgeSelection.enter().call(createEdge)
    edgeSelection.call(applyEdge)

    const applyNode = (g: NodeSelection) => {
      g.attr("transform", ({ x, y }) => `translate(${x},${y})`)

      // Circle
      let circle = g.select<SVGCircleElement>("circle")
      if (circle.empty()) {
        circle = g.append("circle")
      }

      circle.attr("r", nodeRadius).attr("fill", ({ id, relation }) => {
        const weight = relationIsStreamingJob(relation) ? "500" : "400"
        const baseColor = isSelected(id)
          ? theme.colors.blue[weight]
          : theme.colors.gray[weight]
        if (relationStats) {
          const relationId = parseInt(id)
          if (!isNaN(relationId) && relationStats[relationId]) {
            const currentMs = epochToUnixMillis(
              relationStats[relationId].currentEpoch
            )
            return latencyToColor(now_ms - currentMs, baseColor)
          }
        }
        return baseColor
      })

      // Relation name
      let text = g.select<SVGTextElement>(".text")
      if (text.empty()) {
        text = g.append("text").attr("class", "text")
      }

      text
        .attr("fill", "black")
        .text(({ name }) => name)
        .attr("font-family", "inherit")
        .attr("text-anchor", "middle")
        .attr("dy", nodeRadius * 2)
        .attr("font-size", 12)
        .attr("transform", "rotate(-8)")

      // Relation type
      let typeText = g.select<SVGTextElement>(".type")
      if (typeText.empty()) {
        typeText = g.append("text").attr("class", "type")
      }

      const relationTypeAbbr = (relation: Relation) => {
        const type = relationType(relation)
        if (type === "SINK") {
          return "K"
        } else {
          return type.charAt(0)
        }
      }

      typeText
        .attr("fill", "white")
        .text(({ relation }) => `${relationTypeAbbr(relation)}`)
        .attr("font-family", "inherit")
        .attr("text-anchor", "middle")
        .attr("dy", nodeRadius * 0.5)
        .attr("font-size", 16)
        .attr("font-weight", "bold")

      // Tooltip
      const getTooltipContent = (relation: Relation, id: string) => {
        const relationId = parseInt(id)
        const stats = relationStats?.[relationId]
        const latencySeconds = stats
          ? (
              (Date.now() - epochToUnixMillis(stats.currentEpoch)) /
              1000
            ).toFixed(2)
          : "N/A"
        const epoch = stats?.currentEpoch ?? "N/A"

        return `<b>${relation.name} (${relationTypeTitleCase(
          relation
        )})</b><br>Epoch: ${epoch}<br>Latency: ${latencySeconds} seconds`
      }

      g.on("mouseover", (event, { relation, id }) => {
        // Remove existing tooltip if any
        d3.selectAll(".tooltip").remove()

        // Create new tooltip
        d3.select("body")
          .append("div")
          .attr("class", "tooltip")
          .style("position", "absolute")
          .style("background", "white")
          .style("padding", "10px")
          .style("border", "1px solid #ddd")
          .style("border-radius", "4px")
          .style("pointer-events", "none")
          .style("left", event.pageX + 10 + "px")
          .style("top", event.pageY + 10 + "px")
          .style("font-size", "12px")
          .html(getTooltipContent(relation, id))
      })
        .on("mousemove", (event) => {
          d3.select(".tooltip")
            .style("left", event.pageX + 10 + "px")
            .style("top", event.pageY + 10 + "px")
        })
        .on("mouseout", () => {
          d3.selectAll(".tooltip").remove()
        })

      // Relation modal
      g.style("cursor", "pointer").on("click", (_, { relation, id }) => {
        setSelectedId(id)
        setModalId(relation.id)
      })

      return g
    }

    const createNode = (sel: Enter<NodeSelection>) =>
      sel.append("g").attr("class", "node").call(applyNode)

    const g = svgSelection.select(".boxes")
    const nodeSelection = g
      .selectAll<SVGGElement, null>(".node")
      .data(layoutMap)
    type NodeSelection = typeof nodeSelection

    nodeSelection.enter().call(createNode)
    nodeSelection.call(applyNode)
    nodeSelection.exit().remove()
  }, [
    layoutMap,
    links,
    selectedId,
    setModalId,
    setSelectedId,
    backPressures,
    relationStats,
  ])

  return (
    <>
      <svg ref={svgRef} width={`${width}px`} height={`${height}px`}>
        <g className="edges" />
        <g className="boxes" />
      </svg>
      <CatalogModal modalData={modalData} onClose={() => setModalId(null)} />
    </>
  )
}
