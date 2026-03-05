import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Pagination, Pagination$Outbound } from "./pagination.js";
import { Refund, Refund$Outbound } from "./refund.js";
export type ListResourceRefund = {
    items: Array<Refund>;
    pagination: Pagination;
};
/** @internal */
export declare const ListResourceRefund$inboundSchema: z.ZodType<ListResourceRefund, z.ZodTypeDef, unknown>;
/** @internal */
export type ListResourceRefund$Outbound = {
    items: Array<Refund$Outbound>;
    pagination: Pagination$Outbound;
};
/** @internal */
export declare const ListResourceRefund$outboundSchema: z.ZodType<ListResourceRefund$Outbound, z.ZodTypeDef, ListResourceRefund>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ListResourceRefund$ {
    /** @deprecated use `ListResourceRefund$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ListResourceRefund, z.ZodTypeDef, unknown>;
    /** @deprecated use `ListResourceRefund$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ListResourceRefund$Outbound, z.ZodTypeDef, ListResourceRefund>;
    /** @deprecated use `ListResourceRefund$Outbound` instead. */
    type Outbound = ListResourceRefund$Outbound;
}
export declare function listResourceRefundToJSON(listResourceRefund: ListResourceRefund): string;
export declare function listResourceRefundFromJSON(jsonString: string): SafeParseResult<ListResourceRefund, SDKValidationError>;
//# sourceMappingURL=listresourcerefund.d.ts.map