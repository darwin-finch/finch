import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CustomerOrder, CustomerOrder$Outbound } from "./customerorder.js";
import { Pagination, Pagination$Outbound } from "./pagination.js";
export type ListResourceCustomerOrder = {
    items: Array<CustomerOrder>;
    pagination: Pagination;
};
/** @internal */
export declare const ListResourceCustomerOrder$inboundSchema: z.ZodType<ListResourceCustomerOrder, z.ZodTypeDef, unknown>;
/** @internal */
export type ListResourceCustomerOrder$Outbound = {
    items: Array<CustomerOrder$Outbound>;
    pagination: Pagination$Outbound;
};
/** @internal */
export declare const ListResourceCustomerOrder$outboundSchema: z.ZodType<ListResourceCustomerOrder$Outbound, z.ZodTypeDef, ListResourceCustomerOrder>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ListResourceCustomerOrder$ {
    /** @deprecated use `ListResourceCustomerOrder$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ListResourceCustomerOrder, z.ZodTypeDef, unknown>;
    /** @deprecated use `ListResourceCustomerOrder$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ListResourceCustomerOrder$Outbound, z.ZodTypeDef, ListResourceCustomerOrder>;
    /** @deprecated use `ListResourceCustomerOrder$Outbound` instead. */
    type Outbound = ListResourceCustomerOrder$Outbound;
}
export declare function listResourceCustomerOrderToJSON(listResourceCustomerOrder: ListResourceCustomerOrder): string;
export declare function listResourceCustomerOrderFromJSON(jsonString: string): SafeParseResult<ListResourceCustomerOrder, SDKValidationError>;
//# sourceMappingURL=listresourcecustomerorder.d.ts.map