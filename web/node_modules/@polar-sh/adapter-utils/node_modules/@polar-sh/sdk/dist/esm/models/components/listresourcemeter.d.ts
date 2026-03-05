import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Meter, Meter$Outbound } from "./meter.js";
import { Pagination, Pagination$Outbound } from "./pagination.js";
export type ListResourceMeter = {
    items: Array<Meter>;
    pagination: Pagination;
};
/** @internal */
export declare const ListResourceMeter$inboundSchema: z.ZodType<ListResourceMeter, z.ZodTypeDef, unknown>;
/** @internal */
export type ListResourceMeter$Outbound = {
    items: Array<Meter$Outbound>;
    pagination: Pagination$Outbound;
};
/** @internal */
export declare const ListResourceMeter$outboundSchema: z.ZodType<ListResourceMeter$Outbound, z.ZodTypeDef, ListResourceMeter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ListResourceMeter$ {
    /** @deprecated use `ListResourceMeter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ListResourceMeter, z.ZodTypeDef, unknown>;
    /** @deprecated use `ListResourceMeter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ListResourceMeter$Outbound, z.ZodTypeDef, ListResourceMeter>;
    /** @deprecated use `ListResourceMeter$Outbound` instead. */
    type Outbound = ListResourceMeter$Outbound;
}
export declare function listResourceMeterToJSON(listResourceMeter: ListResourceMeter): string;
export declare function listResourceMeterFromJSON(jsonString: string): SafeParseResult<ListResourceMeter, SDKValidationError>;
//# sourceMappingURL=listresourcemeter.d.ts.map