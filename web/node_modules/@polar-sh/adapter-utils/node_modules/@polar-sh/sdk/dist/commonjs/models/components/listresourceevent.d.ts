import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Event, Event$Outbound } from "./event.js";
import { Pagination, Pagination$Outbound } from "./pagination.js";
export type ListResourceEvent = {
    items: Array<Event>;
    pagination: Pagination;
};
/** @internal */
export declare const ListResourceEvent$inboundSchema: z.ZodType<ListResourceEvent, z.ZodTypeDef, unknown>;
/** @internal */
export type ListResourceEvent$Outbound = {
    items: Array<Event$Outbound>;
    pagination: Pagination$Outbound;
};
/** @internal */
export declare const ListResourceEvent$outboundSchema: z.ZodType<ListResourceEvent$Outbound, z.ZodTypeDef, ListResourceEvent>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ListResourceEvent$ {
    /** @deprecated use `ListResourceEvent$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ListResourceEvent, z.ZodTypeDef, unknown>;
    /** @deprecated use `ListResourceEvent$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ListResourceEvent$Outbound, z.ZodTypeDef, ListResourceEvent>;
    /** @deprecated use `ListResourceEvent$Outbound` instead. */
    type Outbound = ListResourceEvent$Outbound;
}
export declare function listResourceEventToJSON(listResourceEvent: ListResourceEvent): string;
export declare function listResourceEventFromJSON(jsonString: string): SafeParseResult<ListResourceEvent, SDKValidationError>;
//# sourceMappingURL=listresourceevent.d.ts.map